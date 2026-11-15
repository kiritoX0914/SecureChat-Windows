//! Port of ChatManager.swift + NetworkManager.swift link state machine.
use crate::crypto::{random32, CryptoEngine};
use crate::model::*;
use crate::net::{self, Beacon, Done, Event, LinkWriter};
use crate::store::{Profile, Store};
use crate::transfer::{Transfers, Writers, CHUNK_SIZE};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

pub const MAX_FILE_SIZE: i64 = 4 * 1024 * 1024 * 1024;
const DEAD_LINK_TIMEOUT: Duration = Duration::from_secs(25);
const TICK: Duration = Duration::from_secs(5);
const SWEEP_CONCURRENCY: usize = 24;

#[derive(Clone, Default)]
pub struct Config {
    pub data_dir: Option<PathBuf>,
    pub port: u16,
    pub device_id: Option<String>,
    pub display_name: Option<String>,
    /// Re-dial remembered addresses once at startup (no periodic scanning).
    pub dial_known_on_start: bool,
    pub enable_beacon: bool,
}

struct Link {
    outgoing: bool,
    is_probe: bool,
    is_manual: bool,
    host: Option<String>,
    peer_id: Option<String>,
    ready: bool,
    activated_at: Option<Instant>,
    last_received: Instant,
    sends_pings: bool,
    writer: Option<LinkWriter>,
    stream: Option<TcpStream>,
    deadline: Option<Instant>,
    sweep_member: bool,
}

enum Pending {
    MessageSent { key: String, msg_id: String, conv_id: String },
}

pub struct Core {
    pub local_device_id: String,
    pub display_name: String,
    pub avatar_data: Option<Vec<u8>>,
    profile_version: i64,

    pub conversations: HashMap<String, Conversation>,
    pub transfer_progress: HashMap<String, f32>,
    pub last_error: Option<String>,

    pub crypto: CryptoEngine,
    pub store: Store,
    known_hosts: HashSet<String>,
    deleted_groups: HashMap<String, i64>,
    v2_peers: HashSet<String>,
    session_ready: HashSet<String>,
    awaiting_init: HashSet<String>,
    last_reset: HashMap<String, Instant>,
    control_queue: HashMap<String, Vec<ControlMessage>>,
    in_flight: HashSet<String>,

    events: Sender<Event>,
    links: HashMap<u64, Link>,
    active: HashMap<String, u64>,
    writers: Writers,
    probing_hosts: HashSet<String>,
    sweep_queue: VecDeque<String>,
    sweep_in_flight: usize,
    next_link: u64,
    next_token: u64,
    pending: HashMap<u64, Pending>,
    transfers: Transfers,
    beacon: Option<Beacon>,
    pub listening_port: Option<u16>,
    pub scan_status: String,
    pub is_scanning: bool,
    pub last_manual_connect_error: Option<String>,
    pub last_manual_connect_success: Option<String>,

    dirty_convs: bool,
    dirty_hosts: bool,
    last_flush: Instant,
    last_tick: Instant,
    notify: Option<Box<dyn Fn() + Send>>,
    pub log_enabled: bool,
}

#[derive(Clone)]
pub struct CoreHandle {
    pub core: Arc<Mutex<Core>>,
    pub events: Sender<Event>,
}

impl CoreHandle {
    pub fn lock(&self) -> MutexGuard<'_, Core> {
        match self.core.lock() { Ok(g) => g, Err(p) => p.into_inner() }
    }
    pub fn wake(&self) { let _ = self.events.send(Event::Ui); }
    pub fn set_notify(&self, f: Box<dyn Fn() + Send>) { self.lock().notify = Some(f); }
}

fn trim(s: &str) -> String { s.trim().to_string() }

impl Core {
    /// Create the core, start listener + engine thread.
    pub fn start(cfg: Config) -> CoreHandle {
        let store = Store::new(cfg.data_dir.clone());
        let mut profile = store.load_profile().unwrap_or_default();
        if let Some(id) = &cfg.device_id { profile.device_id = id.clone(); }
        if profile.device_id.is_empty() { profile.device_id = new_uuid(); }
        if let Some(n) = &cfg.display_name { if profile.display_name.is_empty() || cfg.device_id.is_some() { profile.display_name = n.clone(); } }
        if profile.display_name.trim().is_empty() {
            profile.display_name = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "Windows PC".into());
        }
        store.save_profile(&profile);
        let keys = match store.load_keys() {
            Some(k) => k,
            None => { let k = CryptoEngine::generate_keys(); store.save_keys(&k); k }
        };
        let crypto = CryptoEngine::new(keys, store.load_sessions());
        let mut conversations = store.load_conversations();
        let mut hosts: HashSet<String> = store.load_known_hosts().into_iter().collect();
        for c in conversations.values_mut() {
            c.peer.is_online = false;
            c.is_typing = false;
            if !c.is_group() { if let Some(a) = &c.peer.last_address { hosts.insert(a.clone()); } }
        }
        let deleted_groups = store.load_deleted_groups();

        let (tx, rx) = channel::<Event>();
        let writers: Writers = Arc::new(Mutex::new(HashMap::new()));
        let transfers = Transfers::start(tx.clone(), writers.clone());
        let port = if cfg.port == 0 { net::DEFAULT_TCP_PORT } else { cfg.port };
        let listening_port = net::start_listener(port, tx.clone());
        let beacon = if cfg.enable_beacon { Some(Beacon::start(&profile.device_id, tx.clone())) } else { None };

        let core = Core {
            local_device_id: profile.device_id.clone(),
            display_name: profile.display_name.clone(),
            avatar_data: profile.avatar.clone(),
            profile_version: profile.profile_version,
            conversations,
            transfer_progress: HashMap::new(),
            last_error: None,
            crypto,
            store,
            known_hosts: hosts,
            deleted_groups,
            v2_peers: HashSet::new(),
            session_ready: HashSet::new(),
            awaiting_init: HashSet::new(),
            last_reset: HashMap::new(),
            control_queue: HashMap::new(),
            in_flight: HashSet::new(),
            events: tx.clone(),
            links: HashMap::new(),
            active: HashMap::new(),
            writers,
            probing_hosts: HashSet::new(),
            sweep_queue: VecDeque::new(),
            sweep_in_flight: 0,
            next_link: 1,
            next_token: 1,
            pending: HashMap::new(),
            transfers,
            beacon,
            listening_port,
            scan_status: "Auto scan is off · click Scan to search".into(),
            is_scanning: false,
            last_manual_connect_error: None,
            last_manual_connect_success: None,
            dirty_convs: false,
            dirty_hosts: false,
            last_flush: Instant::now(),
            last_tick: Instant::now(),
            notify: None,
            log_enabled: std::env::var_os("SC_LOG").is_some(),
        };
        let handle = CoreHandle { core: Arc::new(Mutex::new(core)), events: tx };
        {
            let mut c = handle.lock();
            c.save_hosts_now();
            if cfg.dial_known_on_start { c.dial_known_hosts(); }
        }
        let h2 = handle.clone();
        thread::Builder::new().name("engine".into()).spawn(move || engine_loop(h2, rx)).expect("engine");
        handle
    }

    fn log(&self, s: &str) { if self.log_enabled { eprintln!("[core] {s}"); } }

    fn ping_notify(&self) { if let Some(n) = &self.notify { n(); } }

    fn handle_event(&mut self, ev: Event) {
        match ev {
            Event::Incoming(stream) => self.handle_incoming(stream),
            Event::Dialed { link, result } => self.handle_dialed(link, result),
            Event::Frame { link, data } => self.handle_frame(link, data),
            Event::Closed { link } => self.close(link),
            Event::WriteDone { token, ok } => {
                if let Some(Pending::MessageSent { key, msg_id, conv_id }) = self.pending.remove(&token) {
                    self.in_flight.remove(&key);
                    if ok { self.mark_sent(&msg_id, &conv_id) } else { self.refresh_status_after_attempt(&msg_id, &conv_id) }
                }
            }
            Event::TransferProgress { id, progress } => { self.transfer_progress.insert(id, progress); }
            Event::IncomingDone { id, peer, ok } => self.incoming_transfer_finished(&id, &peer, ok),
            Event::OutgoingDone { key, msg_id, conv_id, ok } => {
                self.in_flight.remove(&key);
                self.transfer_progress.remove(&msg_id);
                if ok { self.mark_sent(&msg_id, &conv_id) } else { self.refresh_status_after_attempt(&msg_id, &conv_id) }
            }
            Event::Beacon { host, remote_id } => {
                // Mac-like: no automatic dialing; only answer while a manual scan runs.
                if remote_id != self.local_device_id && self.is_scanning && !self.is_connected(&remote_id) {
                    self.connect(&host, net::DEFAULT_TCP_PORT, true, false, 3);
                }
            }
            Event::Ui => {}
        }
    }

    fn periodic(&mut self) {
        let now = Instant::now();
        let expired: Vec<u64> = self.links.iter().filter(|(_, l)| l.deadline.map(|d| now >= d).unwrap_or(false)).map(|(k, _)| *k).collect();
        for id in expired {
            let unidentified = self.links.get(&id).map(|l| l.peer_id.is_none()).unwrap_or(false);
            if unidentified { self.close(id); } else if let Some(l) = self.links.get_mut(&id) { l.deadline = None; }
        }
        if now.duration_since(self.last_tick) >= TICK {
            self.last_tick = now;
            self.tick();
        }
        if now.duration_since(self.last_flush) >= Duration::from_secs(1) {
            self.last_flush = now;
            self.flush();
        }
    }

    pub fn flush(&mut self) {
        if self.dirty_convs { self.dirty_convs = false; self.store.save_conversations(&self.conversations); }
        if self.crypto.dirty { self.crypto.dirty = false; self.store.save_sessions(&self.crypto.st); }
        if self.dirty_hosts { self.save_hosts_now(); }
    }

    fn save_hosts_now(&mut self) {
        self.dirty_hosts = false;
        let mut v: Vec<String> = self.known_hosts.iter().cloned().collect();
        v.sort();
        self.store.save_known_hosts(&v);
    }

    fn persist(&mut self) { self.dirty_convs = true; }

    fn save_profile(&self) {
        self.store.save_profile(&Profile {
            device_id: self.local_device_id.clone(),
            display_name: self.display_name.clone(),
            avatar: self.avatar_data.clone(),
            profile_version: self.profile_version,
        });
    }

    // Links -------------------------------------------------------------

    fn alloc_link(&mut self) -> u64 { let id = self.next_link; self.next_link += 1; id }

    fn new_link(outgoing: bool, is_probe: bool, is_manual: bool, host: Option<String>) -> Link {
        Link { outgoing, is_probe, is_manual, host, peer_id: None, ready: false, activated_at: None, last_received: Instant::now(), sends_pings: false, writer: None, stream: None, deadline: None, sweep_member: false }
    }

    fn handle_incoming(&mut self, stream: TcpStream) {
        let host = stream.peer_addr().ok().and_then(|a| net::ipv4_host(&a));
        let id = self.alloc_link();
        let mut link = Self::new_link(false, false, false, host);
        link.deadline = Some(Instant::now() + Duration::from_secs(15));
        self.links.insert(id, link);
        self.link_ready(id, stream);
    }

    fn handle_dialed(&mut self, id: u64, result: Result<TcpStream, String>) {
        let Some(l) = self.links.get(&id) else {
            if let Ok(s) = result { let _ = s.shutdown(std::net::Shutdown::Both); }
            return;
        };
        match result {
            Ok(stream) => {
                let probe = l.is_probe;
                self.link_ready(id, stream);
                let secs = if probe { 6 } else { 15 };
                if let Some(l) = self.links.get_mut(&id) { l.deadline = Some(Instant::now() + Duration::from_secs(secs)); }
            }
            Err(e) => {
                if l.is_manual { self.last_manual_connect_error = Some(e); }
                self.close(id);
            }
        }
    }

    fn link_ready(&mut self, id: u64, stream: TcpStream) {
        let Some((writer, stream)) = net::spawn_link(id, stream, self.events.clone()) else { self.close(id); return };
        let hello = self.hello_frame(None);
        writer.send(hello, Done::None);
        if let Some(l) = self.links.get_mut(&id) {
            l.ready = true;
            l.last_received = Instant::now();
            l.writer = Some(writer);
            l.stream = Some(stream);
        }
    }

    fn close(&mut self, id: u64) {
        let Some(link) = self.links.remove(&id) else { return };
        if let Some(s) = &link.stream { let _ = s.shutdown(std::net::Shutdown::Both); }
        if let Some(w) = &link.writer { w.closed.store(true, std::sync::atomic::Ordering::SeqCst); }
        if link.is_probe { if let Some(h) = &link.host { self.probing_hosts.remove(h); } }
        if link.sweep_member {
            self.sweep_in_flight = self.sweep_in_flight.saturating_sub(1);
            self.pump_sweep();
        }
        if let Some(peer) = &link.peer_id {
            if self.active.get(peer) == Some(&id) {
                self.active.remove(peer);
                self.writers.lock().unwrap().remove(peer);
                self.on_peer_disconnected(peer.clone());
            }
        }
    }

    fn identify(&mut self, id: u64, peer_id: String, name: String) {
        let (is_probe, host, sweep) = match self.links.get(&id) { Some(l) => (l.is_probe, l.host.clone(), l.sweep_member), None => return };
        if is_probe { if let Some(h) = &host { self.probing_hosts.remove(h); } }
        if sweep {
            if let Some(l) = self.links.get_mut(&id) { l.sweep_member = false; }
            self.sweep_in_flight = self.sweep_in_flight.saturating_sub(1);
            self.pump_sweep();
        }
        if peer_id == self.local_device_id { self.close(id); return; }
        if let Some(l) = self.links.get_mut(&id) { l.peer_id = Some(peer_id.clone()); l.deadline = None; }
        let now = Instant::now();
        let existing = self.active.get(&peer_id).cloned().filter(|e| *e != id && self.links.contains_key(e));
        if let Some(existing) = existing {
            let ex = &self.links[&existing];
            let fresh = ex.activated_at.map(|t| now.duration_since(t) < Duration::from_secs(3)).unwrap_or(false);
            let mut keep_new = true;
            if fresh {
                // Simultaneous dial: the link initiated by the smaller device id wins.
                let me = self.local_device_id.clone();
                let winner = if me < peer_id { me.clone() } else { peer_id.clone() };
                let new_init = if self.links[&id].outgoing { me.clone() } else { peer_id.clone() };
                let old_init = if ex.outgoing { me.clone() } else { peer_id.clone() };
                if new_init != old_init { keep_new = new_init == winner; }
            }
            if keep_new {
                self.log(&format!("replacing link for {peer_id}"));
                self.active.insert(peer_id.clone(), id);
                if let Some(e) = self.links.get_mut(&existing) { e.peer_id = None; }
                self.close(existing);
            } else {
                self.log(&format!("keeping existing link for {peer_id}"));
                if let Some(l) = self.links.get_mut(&id) { l.peer_id = None; }
                self.close(id);
                return;
            }
        } else {
            self.active.insert(peer_id.clone(), id);
        }
        let manual = {
            let Some(l) = self.links.get_mut(&id) else { return };
            l.activated_at = Some(now);
            if let Some(w) = &l.writer { self.writers.lock().unwrap().insert(peer_id.clone(), w.clone()); }
            l.is_manual
        };
        self.on_peer_found(&peer_id, &name);
        if let Some(h) = host { self.on_peer_address(&peer_id, &h); }
        self.on_peer_connected(&peer_id);
        if manual {
            self.last_manual_connect_error = None;
            self.last_manual_connect_success = Some(format!("Connected to {name}"));
        }
        self.update_scan_idle();
    }

    fn handle_frame(&mut self, id: u64, data: Vec<u8>) {
        let Some(link) = self.links.get_mut(&id) else { return };
        link.last_received = Instant::now();
        if !link.sends_pings && data.len() < 512 && data.first() == Some(&b'{') {
            if let Ok(t) = std::str::from_utf8(&data) { if t.contains("\"type\":7") { link.sends_pings = true; } }
        }
        if let Some(peer) = link.peer_id.clone() {
            self.handle_received_data(data, &peer);
            return;
        }
        let env: Option<WireEnvelope> = if data.first() == Some(&b'{') { serde_json::from_slice(&data).ok() } else { None };
        let Some(env) = env else { self.close(id); return };
        self.identify(id, env.sender_id.clone(), env.sender_name.clone());
        let peer = self.links.get(&id).and_then(|l| l.peer_id.clone());
        if let Some(peer) = peer { self.handle_received_data(data, &peer); }
    }

    fn send_raw(&mut self, data: Vec<u8>, peer: &str, done: Done) -> bool {
        let w = self.active.get(peer).and_then(|id| self.links.get(id)).and_then(|l| l.writer.clone());
        match w {
            Some(w) => w.send(data, done),
            None => {
                if let Done::Token(t) = done { let _ = self.events.send(Event::WriteDone { token: t, ok: false }); }
                false
            }
        }
    }

    pub fn is_connected(&self, peer: &str) -> bool { self.active.contains_key(peer) }

    pub fn connected_peer_ids(&self) -> Vec<String> { self.active.keys().cloned().collect() }

    fn connect(&mut self, host: &str, port: u16, probe: bool, manual: bool, timeout: u64) {
        let own = net::own_addresses();
        if own.iter().any(|a| a == host) && !manual { return; }
        if probe {
            if self.probing_hosts.contains(host) { return; }
            if self.links.values().any(|l| l.host.as_deref() == Some(host) && (l.peer_id.is_some() || !l.ready)) { return; }
        }
        let id = self.alloc_link();
        self.links.insert(id, Self::new_link(true, probe, manual, Some(host.to_string())));
        if probe { self.probing_hosts.insert(host.to_string()); }
        net::dial(id, host.to_string(), port, Duration::from_secs(timeout), self.events.clone());
    }

    /// Manual "Connect by IP".
    pub fn connect_by_ip(&mut self, host: &str, port: u16) {
        let h = host.trim();
        if h.is_empty() || port == 0 { return; }
        self.last_manual_connect_error = None;
        self.last_manual_connect_success = None;
        self.connect(h, port, false, true, 8);
    }

    /// Manual scan: re-dial remembered addresses + sweep the local /24(s) + one UDP beacon.
    pub fn scan_now(&mut self) {
        self.dial_known_hosts();
        self.start_sweep();
        if let Some(b) = &self.beacon { b.announce(); }
    }

    fn dial_known_hosts(&mut self) {
        let connected: HashSet<String> = self.links.values().filter(|l| l.peer_id.is_some()).filter_map(|l| l.host.clone()).collect();
        let hosts: Vec<String> = self.known_hosts.iter().filter(|h| !connected.contains(*h) && !self.probing_hosts.contains(*h)).cloned().collect();
        for h in hosts { self.connect(&h, net::DEFAULT_TCP_PORT, true, false, 3); }
    }

    fn start_sweep(&mut self) {
        let ifaces = net::ipv4_interfaces();
        let own: HashSet<u32> = ifaces.iter().map(|i| u32::from(i.ip)).collect();
        let connected: HashSet<String> = self.links.values().filter(|l| l.peer_id.is_some()).filter_map(|l| l.host.clone()).collect();
        let mut targets = vec![];
        let mut seen = HashSet::new();
        for i in &ifaces {
            let mut mask = u32::from(i.netmask);
            if mask < 0xFFFF_FF00 { mask = 0xFFFF_FF00; }
            let network = u32::from(i.ip) & mask;
            let bcast = network | !mask;
            if bcast <= network + 1 { continue; }
            for h in (network + 1)..bcast {
                if own.contains(&h) { continue; }
                let t = std::net::Ipv4Addr::from(h).to_string();
                if !connected.contains(&t) && seen.insert(t.clone()) { targets.push(t); }
            }
        }
        if targets.is_empty() {
            self.is_scanning = false;
            self.scan_status = "No scannable network".into();
            return;
        }
        self.scan_status = format!("Scanning {} addresses…", targets.len());
        self.is_scanning = true;
        self.sweep_queue = targets.into();
        self.pump_sweep();
    }

    fn pump_sweep(&mut self) {
        while self.sweep_in_flight < SWEEP_CONCURRENCY {
            let Some(host) = self.sweep_queue.pop_front() else { break };
            if self.probing_hosts.contains(&host) { continue; }
            let id = self.alloc_link();
            let mut l = Self::new_link(true, true, false, Some(host.clone()));
            l.sweep_member = true;
            self.links.insert(id, l);
            self.probing_hosts.insert(host.clone());
            self.sweep_in_flight += 1;
            net::dial(id, host, net::DEFAULT_TCP_PORT, Duration::from_secs(2), self.events.clone());
        }
        if self.sweep_queue.is_empty() && self.sweep_in_flight == 0 && self.is_scanning {
            self.is_scanning = false;
            self.update_scan_idle();
        }
    }

    fn update_scan_idle(&mut self) {
        if self.is_scanning { return; }
        let n = self.active.len();
        self.scan_status = if n > 0 { format!("{n} peer(s) connected") } else { "No peers found · click Scan to try again".into() };
    }

    fn tick(&mut self) {
        let ping = self.ping_frame();
        let now = Instant::now();
        let ids: Vec<u64> = self.links.iter().filter(|(_, l)| l.peer_id.is_some()).map(|(k, _)| *k).collect();
        for id in ids {
            let (dead, w) = {
                let l = &self.links[&id];
                (l.sends_pings && now.duration_since(l.last_received) > DEAD_LINK_TIMEOUT, l.writer.clone())
            };
            if dead { self.log("link silent - dropping"); self.close(id); continue; }
            if let Some(w) = w { w.send(ping.clone(), Done::None); }
        }
    }

    // Frames ---------------------------------------------------------------

    fn envelope<T: serde::Serialize>(&self, kind: u8, payload: &T) -> Vec<u8> {
        WireEnvelope::make(kind, &self.local_device_id, &self.display_name, payload)
    }

    fn hello_frame(&self, has_session: Option<bool>) -> Vec<u8> {
        let mut b = self.crypto.pre_key_bundle();
        b.has_session = has_session;
        b.protocol_version = Some(2);
        self.envelope(TYPE_HELLO, &b)
    }

    fn ping_frame(&self) -> Vec<u8> { self.envelope(TYPE_PING, &PingPayload { t: now_unix() }) }

    // Network callbacks -----------------------------------------------------

    fn on_peer_found(&mut self, peer: &str, name: &str) {
        if peer == self.local_device_id || peer.starts_with("group-") { return; }
        if let Some(c) = self.conversations.get_mut(peer) {
            if !name.is_empty() { c.peer.display_name = name.to_string(); }
            c.peer.last_seen = now_ref();
        } else {
            self.conversations.insert(peer.to_string(), Conversation::new(peer, Peer::new(peer, name)));
        }
        self.persist();
    }

    fn on_peer_disconnected(&mut self, peer: String) {
        self.session_ready.remove(&peer);
        if let Some(c) = self.conversations.get_mut(&peer) {
            c.peer.is_online = false;
            c.peer.last_seen = now_ref();
            c.is_typing = false;
            self.persist();
        }
        self.update_scan_idle();
    }

    fn on_peer_connected(&mut self, peer: &str) {
        self.set_online(peer, true);
        self.session_ready.remove(peer);
        let has = self.crypto.has_session(peer) && !self.awaiting_init.contains(peer);
        let hello = self.hello_frame(Some(has));
        self.send_raw(hello, peer, Done::None);
    }

    fn on_peer_address(&mut self, peer: &str, host: &str) {
        if let Some(c) = self.conversations.get_mut(peer) {
            if c.peer.last_address.as_deref() != Some(host) {
                c.peer.last_address = Some(host.to_string());
                self.dirty_convs = true;
            }
        }
        if !self.known_hosts.contains(host) {
            self.known_hosts.insert(host.to_string());
            self.dirty_hosts = true;
        }
    }

    fn set_online(&mut self, peer: &str, online: bool) {
        if let Some(c) = self.conversations.get_mut(peer) {
            if c.peer.is_online != online { c.peer.is_online = online; c.peer.last_seen = now_ref(); }
        }
    }

    // Receiving ----------------------------------------------------------

    fn handle_received_data(&mut self, data: Vec<u8>, peer: &str) {
        if data.first() == Some(&chunk_frame::MAGIC) {
            self.transfers.chunk(peer, data);
            return;
        }
        let Ok(env) = serde_json::from_slice::<WireEnvelope>(&data) else { return };
        if !env.known_type() { return; }
        self.dispatch(env, peer);
    }

    fn dispatch(&mut self, env: WireEnvelope, peer: &str) {
        if env.kind == TYPE_PING { return; }
        self.ensure_conversation(peer, &env.sender_name);
        let online = self.is_connected(peer);
        if let Some(c) = self.conversations.get_mut(peer) {
            if !env.sender_name.is_empty() && (c.peer.display_name != env.sender_name || !c.peer.is_online) {
                c.peer.display_name = env.sender_name.clone();
                c.peer.is_online = online;
            }
        }
        match env.kind {
            TYPE_HELLO => self.handle_hello(&env, peer),
            TYPE_PREKEY_MESSAGE => self.handle_pre_key_message(&env, peer),
            TYPE_MESSAGE => self.handle_encrypted_message(&env, peer),
            TYPE_ACK => self.handle_ack(&env, peer),
            TYPE_TYPING => self.handle_typing(&env, peer),
            _ => {}
        }
    }

    /// Deterministic role: the smaller device id initiates X3DH.
    fn am_smaller(&self, peer: &str) -> bool { self.local_device_id.as_str() < peer }

    fn handle_hello(&mut self, env: &WireEnvelope, peer: &str) {
        let Ok(bundle) = serde_json::from_slice::<PreKeyBundlePayload>(&env.payload) else { return };
        if let Some(c) = self.conversations.get_mut(peer) {
            c.pre_key_bundle = Some(bundle.clone());
            c.peer.identity_dh_key_data = Some(bundle.identity_dh_key.clone());
            c.peer.identity_signing_key_data = Some(bundle.identity_signing_key.clone());
            c.peer.is_online = true;
            self.dirty_convs = true;
        }
        if bundle.protocol_version.unwrap_or(0) < 2 {
            self.v2_peers.remove(peer);
            if !self.crypto.has_session(peer) {
                let h = self.hello_frame(None);
                self.send_raw(h, peer, Done::None);
            }
            self.mark_ready(peer);
            return;
        }
        self.v2_peers.insert(peer.to_string());
        // Identification-only hello (first frame on a link); the full one follows.
        let Some(theirs) = bundle.has_session else { return };
        if self.am_smaller(peer) {
            if !self.crypto.has_session(peer) || !theirs {
                self.crypto.reset_session(peer);
                self.initiate_session(peer, &bundle);
            } else {
                self.mark_ready(peer);
            }
        } else {
            if !theirs {
                self.crypto.reset_session(peer);
                self.awaiting_init.insert(peer.to_string());
            }
            if self.crypto.has_session(peer) && !self.awaiting_init.contains(peer) {
                self.mark_ready(peer);
            } else {
                self.awaiting_init.insert(peer.to_string());
            }
        }
    }

    fn initiate_session(&mut self, peer: &str, bundle: &PreKeyBundlePayload) {
        let r = (|| -> Result<(), String> {
            self.crypto.process_received_bundle(bundle, peer).map_err(|e| e.to_string())?;
            let inner = InnerEnvelope { v: 2, control: ControlMessage::new(ControlKind::SessionInit) };
            let pt = serde_json::to_vec(&inner).map_err(|e| e.to_string())?;
            let (pre, payload) = self.crypto.encrypt(&pt, peer).map_err(|e| e.to_string())?;
            let env = WireEnvelope::raw(if pre { TYPE_PREKEY_MESSAGE } else { TYPE_MESSAGE }, &self.local_device_id, &self.display_name, payload);
            self.send_raw(env, peer, Done::None);
            Ok(())
        })();
        match r {
            Ok(()) => self.mark_ready(peer),
            Err(e) => self.log(&format!("initiateSession failed: {e}")),
        }
    }

    fn mark_ready(&mut self, peer: &str) {
        self.awaiting_init.remove(peer);
        let newly = self.session_ready.insert(peer.to_string());
        if newly {
            let p = self.profile_control();
            self.send_control(p, peer);
            let reqs: Vec<String> = self.conversations.values()
                .filter_map(|c| c.group.as_ref())
                .filter(|g| g.version == 0 && g.members.iter().any(|m| m == peer))
                .map(|g| g.id.clone()).collect();
            for gid in reqs {
                let mut c = ControlMessage::new(ControlKind::GroupInfoRequest);
                c.group_id = Some(gid);
                self.send_control(c, peer);
            }
        }
        self.flush_control_queue(peer);
        self.flush_outbox(peer);
    }

    fn handle_decrypt_failure(&mut self, peer: &str, err: String) {
        self.log(&format!("decrypt failure from {peer}: {err}"));
        let now = Instant::now();
        if let Some(t) = self.last_reset.get(peer) { if now.duration_since(*t) < Duration::from_secs(8) { return; } }
        self.last_reset.insert(peer.to_string(), now);
        self.crypto.reset_session(peer);
        self.session_ready.remove(peer);
        if !self.v2_peers.contains(peer) {
            let h = self.hello_frame(None);
            self.send_raw(h, peer, Done::None);
            return;
        }
        let bundle = self.conversations.get(peer).and_then(|c| c.pre_key_bundle.clone());
        match bundle {
            Some(b) if self.am_smaller(peer) => self.initiate_session(peer, &b),
            _ => {
                self.awaiting_init.insert(peer.to_string());
                let h = self.hello_frame(Some(false));
                self.send_raw(h, peer, Done::None);
            }
        }
    }

    fn handle_pre_key_message(&mut self, env: &WireEnvelope, peer: &str) {
        let r = serde_json::from_slice::<PreKeyMessagePayload>(&env.payload).map_err(|e| e.to_string())
            .and_then(|m| self.crypto.decrypt_pre_key_message(&m, peer).map_err(|e| e.to_string()));
        match r {
            Ok(pt) => {
                self.awaiting_init.remove(peer);
                self.handle_plaintext(&pt, peer);
                self.mark_ready(peer);
            }
            Err(e) => self.handle_decrypt_failure(peer, e),
        }
    }

    fn handle_encrypted_message(&mut self, env: &WireEnvelope, peer: &str) {
        let r = serde_json::from_slice::<EncryptedMessagePayload>(&env.payload).map_err(|e| e.to_string())
            .and_then(|m| self.crypto.decrypt_message(&m, peer).map_err(|e| e.to_string()));
        match r {
            Ok(pt) => self.handle_plaintext(&pt, peer),
            Err(e) => self.handle_decrypt_failure(peer, e),
        }
    }

    fn handle_plaintext(&mut self, pt: &[u8], peer: &str) {
        if let Ok(m) = serde_json::from_slice::<ChatMessage>(pt) {
            self.handle_incoming_chat(m, peer);
        } else if let Ok(inner) = serde_json::from_slice::<InnerEnvelope>(pt) {
            self.handle_control(inner.control, peer);
        } else {
            self.log("undecodable plaintext");
        }
    }

    fn handle_incoming_chat(&mut self, incoming: ChatMessage, peer: &str) {
        if incoming.sender_id != peer { return; }
        let conv_id = match &incoming.group_id {
            Some(g) => { self.ensure_group_placeholder(g, peer); g.clone() }
            None => peer.to_string(),
        };
        if !self.conversations.contains_key(&conv_id) { return; }
        let existing = self.conversations[&conv_id].messages.iter().position(|m| m.id == incoming.id);
        if let Some(idx) = existing {
            let ex = self.conversations[&conv_id].messages[idx].clone();
            let restart = ex.attachment.as_ref().map(|a| a.is_complete == Some(false)).unwrap_or(false)
                && incoming.attachment.as_ref().map(|a| a.file_key.is_some()).unwrap_or(false);
            if restart {
                let a = self.register_incoming_transfer(incoming.attachment.clone().unwrap(), &incoming.id, peer);
                self.conversations.get_mut(&conv_id).unwrap().messages[idx].attachment = Some(a);
                self.persist();
            } else {
                self.send_ack(&incoming.id, STATUS_DELIVERED, peer, incoming.group_id.clone());
            }
            return;
        }
        let mut m = incoming;
        m.pending_recipients = None;
        m.status = STATUS_DELIVERED;
        let mut needs_transfer = false;
        if let Some(mut a) = m.attachment.take() {
            if let Some(data) = a.data.take() {
                match self.store.write(&data, &m.id, &a.filename) {
                    Some(rel) => a.local_file = Some(rel),
                    None => a.data = Some(data),
                }
                a.is_complete = Some(true);
                a.file_key = None;
            } else if a.transfer_id.is_some() && a.file_key.is_some() {
                a = self.register_incoming_transfer(a, &m.id, peer);
                needs_transfer = a.is_complete == Some(false);
            }
            m.attachment = Some(a);
        }
        let (id, gid) = (m.id.clone(), m.group_id.clone());
        let c = self.conversations.get_mut(&conv_id).unwrap();
        c.messages.push(m);
        c.unread_count += 1;
        c.is_typing = false;
        self.persist();
        if !needs_transfer { self.send_ack(&id, STATUS_DELIVERED, peer, gid); }
    }

    fn register_incoming_transfer(&mut self, wire: Attachment, message_id: &str, peer: &str) -> Attachment {
        let mut a = wire;
        let (rel, path) = self.store.prepare(message_id, &a.filename);
        a.local_file = Some(rel);
        a.data = None;
        let total = a.chunk_count.unwrap_or(0).max(0) as u32;
        if let Some(key) = a.file_key.take() {
            let cs = a.chunk_size.unwrap_or(CHUNK_SIZE as i64).max(1) as usize;
            self.transfers.register(message_id, peer, key, total, cs, path);
        }
        a.is_complete = Some(total == 0);
        if total > 0 { self.transfer_progress.insert(message_id.to_string(), 0.0); }
        a
    }

    fn incoming_transfer_finished(&mut self, id: &str, peer: &str, ok: bool) {
        self.transfer_progress.remove(id);
        let mut ack = None;
        for c in self.conversations.values_mut() {
            if let Some(m) = c.messages.iter_mut().find(|m| m.id == id) {
                if ok {
                    if let Some(a) = m.attachment.as_mut() { a.is_complete = Some(true); }
                    ack = Some(m.group_id.clone());
                }
                break;
            }
        }
        if let Some(gid) = ack {
            self.persist();
            self.send_ack(id, STATUS_DELIVERED, peer, gid);
        }
    }

    fn handle_control(&mut self, control: ControlMessage, peer: &str) {
        match control.kind {
            ControlKind::SessionInit => {}
            ControlKind::Profile => {
                let Some(c) = self.conversations.get_mut(peer) else { return };
                if let Some(n) = control.name.as_deref().map(trim) { if !n.is_empty() { c.peer.display_name = n; } }
                c.peer.avatar_data = control.avatar.clone();
                let name = c.peer.display_name.clone();
                for g in self.conversations.values_mut() {
                    if let Some(gi) = g.group.as_mut() {
                        if gi.members.iter().any(|m| m == peer) { gi.member_names.insert(peer.to_string(), name.clone()); }
                    }
                }
                self.persist();
            }
            ControlKind::Delete | ControlKind::Clear => {
                let conv_id = control.group_id.clone().unwrap_or_else(|| peer.to_string());
                let Some(c) = self.conversations.get(&conv_id) else { return };
                if let Some(g) = &c.group { if !g.members.iter().any(|m| m == peer) { return; } }
                let ids: HashSet<String> = if control.kind == ControlKind::Delete {
                    control.ids.clone().unwrap_or_default().into_iter().collect()
                } else {
                    c.messages.iter().map(|m| m.id.clone()).collect()
                };
                self.remove_messages(&ids, &conv_id);
                self.persist();
            }
            ControlKind::GroupUpdate => {
                let Some(g) = control.group else { return };
                if !(g.members.iter().any(|m| m == peer) || g.created_by == peer) { return; }
                self.apply_group_update(g);
            }
            ControlKind::GroupLeave => {
                let Some(gid) = control.group_id else { return };
                if let Some(g) = self.conversations.get_mut(&gid).and_then(|c| c.group.as_mut()) {
                    g.members.retain(|m| m != peer);
                    self.dirty_convs = true;
                }
            }
            ControlKind::GroupInfoRequest => {
                let Some(gid) = control.group_id else { return };
                let Some(g) = self.conversations.get(&gid).and_then(|c| c.group.clone()) else { return };
                if !g.members.iter().any(|m| m == peer) || g.version <= 0 { return; }
                let mut c = ControlMessage::new(ControlKind::GroupUpdate);
                c.group_id = Some(gid);
                c.group = Some(g);
                self.send_control(c, peer);
            }
        }
    }

    fn remove_messages(&mut self, ids: &HashSet<String>, conv_id: &str) {
        if ids.is_empty() { return; }
        let me = self.local_device_id.clone();
        let Some(c) = self.conversations.get_mut(conv_id) else { return };
        for m in c.messages.iter().filter(|m| ids.contains(&m.id)) {
            if m.attachment.is_some() {
                self.transfers.cancel(&m.id);
                self.store.delete(&m.id);
                self.transfer_progress.remove(&m.id);
            }
        }
        c.messages.retain(|m| !ids.contains(&m.id));
        let incoming = c.messages.iter().filter(|m| m.sender_id != me).count() as i64;
        c.unread_count = c.unread_count.min(incoming);
    }

    fn handle_ack(&mut self, env: &WireEnvelope, peer: &str) {
        let Ok(ack) = serde_json::from_slice::<AckPayload>(&env.payload) else { return };
        let mut conv_id = ack.group_id.clone().unwrap_or_else(|| peer.to_string());
        let has = self.conversations.get(&conv_id).map(|c| c.messages.iter().any(|m| m.id == ack.message_id)).unwrap_or(false);
        if !has {
            match self.conversations.iter().find(|(_, c)| c.messages.iter().any(|m| m.id == ack.message_id)) {
                Some((k, _)) => conv_id = k.clone(),
                None => return,
            }
        }
        let Some(c) = self.conversations.get_mut(&conv_id) else { return };
        let Some(m) = c.messages.iter_mut().find(|m| m.id == ack.message_id) else { return };
        if let Some(p) = m.pending_recipients.as_mut() {
            p.retain(|x| x != peer);
            if p.is_empty() { m.pending_recipients = None; }
        }
        if (ack.status > m.status || m.status == STATUS_SENDING || m.status == STATUS_FAILED) && ack.status != STATUS_FAILED {
            m.status = ack.status;
        }
        self.persist();
    }

    fn handle_typing(&mut self, env: &WireEnvelope, peer: &str) {
        let Ok(t) = serde_json::from_slice::<TypingPayload>(&env.payload) else { return };
        if let Some(c) = self.conversations.get_mut(peer) { c.is_typing = t.is_typing; }
    }

    // Send API -----------------------------------------------------------

    fn base_message(&self, conv_id: &str, text: String, content_type: u8, attachment: Option<Attachment>, id: String) -> ChatMessage {
        ChatMessage {
            id, sender_id: self.local_device_id.clone(), recipient_id: conv_id.into(), text,
            timestamp: now_ref(), status: STATUS_SENDING, content_type, attachment,
            reply_to_id: None, reply_to_text: None, reply_to_sender: None, group_id: None, sender_name: None, pending_recipients: None,
        }
    }

    pub fn send_message(&mut self, text: &str, conv_id: &str, reply_to: Option<&str>) {
        let t = text.trim();
        if t.is_empty() { return; }
        let mut m = self.base_message(conv_id, t.into(), CONTENT_TEXT, None, new_uuid());
        self.apply_reply(&mut m, reply_to, conv_id);
        self.deliver(m, conv_id);
    }

    /// Send an image (any format) as inline JPEG like the Swift app.
    pub fn send_image_bytes(&mut self, bytes: &[u8], filename: &str, conv_id: &str, reply_to: Option<&str>) {
        match crate::media::prepare_photo_bytes(bytes) {
            Ok(p) => self.send_prepared_image(p, filename, conv_id, reply_to),
            Err(e) => self.last_error = Some(format!("Unsupported image: {e}")),
        }
    }

    pub fn send_prepared_image(&mut self, p: crate::media::PreparedImage, filename: &str, conv_id: &str, reply_to: Option<&str>) {
        let id = new_uuid();
        let lower = filename.to_lowercase();
        let name = if lower.ends_with(".jpg") || lower.ends_with(".jpeg") { filename.to_string() } else {
            format!("{}.jpg", filename.rsplit_once('.').map(|(s, _)| s).unwrap_or(filename))
        };
        let mut a = Attachment {
            filename: name.clone(), mime_type: "image/jpeg".into(), file_size: p.jpeg.len() as i64,
            width: Some(p.width as i64), height: Some(p.height as i64), thumbnail_data: p.thumbnail, ..Default::default()
        };
        a.local_file = self.store.write(&p.jpeg, &id, &name);
        a.is_complete = Some(true);
        if a.local_file.is_none() { a.data = Some(p.jpeg); }
        let mut m = self.base_message(conv_id, "📷 Photo".into(), CONTENT_IMAGE, Some(a), id);
        self.apply_reply(&mut m, reply_to, conv_id);
        self.deliver(m, conv_id);
    }

    /// Send any file (document, video, …) with chunked E2E transfer.
    pub fn send_file(&mut self, path: &Path, conv_id: &str, reply_to: Option<&str>) {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
        let size = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(-1);
        if size < 0 { self.last_error = Some(format!("Could not read {name}")); return; }
        if size > MAX_FILE_SIZE { self.last_error = Some(format!("{name} is larger than 4 GB")); return; }
        let id = new_uuid();
        let Some(rel) = self.store.copy_in(path, &id, &name) else { self.last_error = Some(format!("Could not read {name}")); return };
        let mime = crate::media::mime_for(&name);
        let is_video = mime.starts_with("video/");
        let a = Attachment {
            filename: name.clone(), mime_type: mime, file_size: size, local_file: Some(rel), is_complete: Some(true),
            transfer_id: Some(id.clone()), chunk_size: Some(CHUNK_SIZE as i64),
            chunk_count: Some((size + CHUNK_SIZE as i64 - 1) / CHUNK_SIZE as i64), ..Default::default()
        };
        let text = if is_video { "🎬 Video".to_string() } else { format!("📎 {name}") };
        let mut m = self.base_message(conv_id, text, CONTENT_FILE, Some(a), id);
        self.apply_reply(&mut m, reply_to, conv_id);
        self.deliver(m, conv_id);
    }

    pub fn resend(&mut self, msg_id: &str, conv_id: &str) {
        let Some(c) = self.conversations.get(conv_id) else { return };
        let Some(m) = c.messages.iter().find(|m| m.id == msg_id) else { return };
        let targets = m.pending_recipients.clone().unwrap_or_else(|| self.recipients(c));
        if let Some(mm) = self.conversations.get_mut(conv_id).and_then(|c| c.messages.iter_mut().find(|m| m.id == msg_id)) {
            if mm.status == STATUS_FAILED { mm.status = STATUS_SENDING; }
        }
        for p in targets { self.transmit(msg_id, conv_id, &p); }
    }

    fn apply_reply(&self, m: &mut ChatMessage, reply_to: Option<&str>, conv_id: &str) {
        let Some(rid) = reply_to else { return };
        let Some(r) = self.conversations.get(conv_id).and_then(|c| c.messages.iter().find(|m| m.id == rid)) else { return };
        m.reply_to_id = Some(r.id.clone());
        m.reply_to_text = Some(r.preview_text().chars().take(140).collect());
        m.reply_to_sender = Some(self.sender_name(r, conv_id));
    }

    fn recipients(&self, c: &Conversation) -> Vec<String> {
        match &c.group {
            Some(g) => g.members.iter().filter(|m| **m != self.local_device_id).cloned().collect(),
            None => vec![c.id.clone()],
        }
    }

    fn deliver(&mut self, original: ChatMessage, conv_id: &str) {
        let Some(c) = self.conversations.get(conv_id) else { return };
        if let Some(g) = &c.group {
            if !g.members.contains(&self.local_device_id) { self.last_error = Some("You are no longer a member of this group.".into()); return; }
        }
        let mut m = original;
        m.sender_name = Some(self.display_name.clone());
        if c.is_group() { m.group_id = Some(conv_id.to_string()); }
        let targets = self.recipients(c);
        m.pending_recipients = if targets.is_empty() { None } else { Some(targets.clone()) };
        m.status = if targets.is_empty() { STATUS_SENT } else { STATUS_SENDING };
        let id = m.id.clone();
        self.conversations.get_mut(conv_id).unwrap().messages.push(m);
        self.persist();
        for p in targets { self.transmit(&id, conv_id, &p); }
    }

    fn can_send(&self, peer: &str) -> bool {
        if !self.is_connected(peer) { return false; }
        if self.v2_peers.contains(peer) { return self.session_ready.contains(peer); }
        self.crypto.has_session(peer) || self.conversations.get(peer).and_then(|c| c.pre_key_bundle.as_ref()).is_some()
    }

    fn transmit(&mut self, msg_id: &str, conv_id: &str, peer: &str) {
        let key = format!("{msg_id}|{peer}");
        if self.in_flight.contains(&key) || !self.can_send(peer) { return; }
        let Some(message) = self.conversations.get(conv_id).and_then(|c| c.messages.iter().find(|m| m.id == msg_id)).cloned() else { return };
        let mut wire = message.clone();
        wire.pending_recipients = None;
        wire.status = STATUS_SENT;
        let mut chunked: Option<(PathBuf, Vec<u8>, u32)> = None;
        if let Some(mut a) = wire.attachment.take() {
            if message.content_type == CONTENT_IMAGE {
                if a.data.is_none() { if let Some(rel) = &a.local_file { a.data = std::fs::read(self.store.location(rel)).ok(); } }
                a.local_file = None;
                a.is_complete = None;
                a.transfer_id = None;
                wire.attachment = Some(a);
            } else if let Some(rel) = a.local_file.clone() {
                let file_key = random32().to_vec();
                a.file_key = Some(file_key.clone());
                a.transfer_id = Some(message.id.clone());
                a.local_file = None;
                a.is_complete = None;
                a.data = None;
                let count = a.chunk_count.unwrap_or((a.file_size + CHUNK_SIZE as i64 - 1) / CHUNK_SIZE as i64);
                a.chunk_count = Some(count);
                a.chunk_size = Some(CHUNK_SIZE as i64);
                wire.attachment = Some(a);
                chunked = Some((self.store.location(&rel), file_key, count.max(0) as u32));
            } else {
                wire.attachment = Some(a);
            }
        }
        let Ok(pt) = serde_json::to_vec(&wire) else { return };
        self.in_flight.insert(key.clone());
        let done = if chunked.is_none() {
            let t = self.next_token;
            self.next_token += 1;
            self.pending.insert(t, Pending::MessageSent { key: key.clone(), msg_id: msg_id.into(), conv_id: conv_id.into() });
            Done::Token(t)
        } else { Done::None };
        if !self.encrypt_and_send(&pt, peer, done) {
            self.in_flight.remove(&key);
            self.refresh_status_after_attempt(msg_id, conv_id);
            return;
        }
        if let Some((path, k, count)) = chunked {
            self.transfer_progress.insert(msg_id.to_string(), 0.0);
            self.transfers.send(path, msg_id.to_string(), k, count, peer.to_string(), key, conv_id.to_string());
        }
    }

    fn mark_sent(&mut self, msg_id: &str, conv_id: &str) {
        if let Some(m) = self.conversations.get_mut(conv_id).and_then(|c| c.messages.iter_mut().find(|m| m.id == msg_id)) {
            if m.status == STATUS_SENDING || m.status == STATUS_FAILED { m.status = STATUS_SENT; self.dirty_convs = true; }
        }
    }

    fn refresh_status_after_attempt(&mut self, msg_id: &str, conv_id: &str) {
        let any = self.in_flight.iter().any(|k| k.starts_with(msg_id));
        if let Some(m) = self.conversations.get_mut(conv_id).and_then(|c| c.messages.iter_mut().find(|m| m.id == msg_id)) {
            if m.status == STATUS_SENDING && !any { m.status = STATUS_FAILED; self.dirty_convs = true; }
        }
    }

    fn flush_outbox(&mut self, peer: &str) {
        let me = self.local_device_id.clone();
        let mut todo = vec![];
        for c in self.conversations.values() {
            for m in &c.messages {
                if m.sender_id == me && m.pending_recipients.as_ref().map(|p| p.iter().any(|x| x == peer)).unwrap_or(false) {
                    todo.push((m.id.clone(), c.id.clone(), m.timestamp));
                }
            }
        }
        todo.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
        for (m, c, _) in todo { self.transmit(&m, &c, peer); }
    }

    fn encrypt_and_send(&mut self, pt: &[u8], peer: &str, done: Done) -> bool {
        let r = (|| -> Result<Vec<u8>, String> {
            if !self.crypto.has_session(peer) {
                if self.v2_peers.contains(peer) { return Err("no session".into()); }
                let b = self.conversations.get(peer).and_then(|c| c.pre_key_bundle.clone()).ok_or("no bundle")?;
                self.crypto.process_received_bundle(&b, peer).map_err(|e| e.to_string())?;
            }
            let (pre, payload) = self.crypto.encrypt(pt, peer).map_err(|e| e.to_string())?;
            Ok(WireEnvelope::raw(if pre { TYPE_PREKEY_MESSAGE } else { TYPE_MESSAGE }, &self.local_device_id, &self.display_name, payload))
        })();
        match r {
            Ok(data) => { self.send_raw(data, peer, done); true }
            Err(e) => {
                self.log(&format!("encrypt failed for {peer}: {e}"));
                if let Done::Token(t) = done { self.pending.remove(&t); }
                false
            }
        }
    }

    fn send_control(&mut self, control: ControlMessage, peer: &str) {
        if peer == self.local_device_id { return; }
        if self.can_send(peer) && self.v2_peers.contains(peer) {
            if let Ok(pt) = serde_json::to_vec(&InnerEnvelope { v: 2, control: control.clone() }) {
                if self.encrypt_and_send(&pt, peer, Done::None) { return; }
            }
        }
        let q = self.control_queue.entry(peer.to_string()).or_default();
        if control.kind == ControlKind::Profile { q.retain(|c| c.kind != ControlKind::Profile); }
        q.push(control);
        if q.len() > 200 { let n = q.len() - 200; q.drain(0..n); }
    }

    fn flush_control_queue(&mut self, peer: &str) {
        if !self.v2_peers.contains(peer) { return; }
        let Some(q) = self.control_queue.remove(peer) else { return };
        for c in q { self.send_control(c, peer); }
    }

    pub fn send_typing(&mut self, conv_id: &str, typing: bool) {
        if self.conversations.get(conv_id).map(|c| c.is_group()).unwrap_or(true) || !self.is_connected(conv_id) { return; }
        let d = self.envelope(TYPE_TYPING, &TypingPayload { is_typing: typing });
        self.send_raw(d, conv_id, Done::None);
    }

    pub fn mark_as_read(&mut self, conv_id: &str) {
        let me = self.local_device_id.clone();
        let Some(c) = self.conversations.get_mut(conv_id) else { return };
        if c.unread_count <= 0 { return; }
        c.unread_count = 0;
        let is_group = c.is_group();
        let mut acks = vec![];
        for m in c.messages.iter_mut() {
            if m.sender_id != me && m.status < STATUS_READ {
                m.status = STATUS_READ;
                acks.push((m.id.clone(), m.sender_id.clone()));
            }
        }
        self.persist();
        for (id, sender) in acks { self.send_ack(&id, STATUS_READ, &sender, if is_group { Some(conv_id.to_string()) } else { None }); }
    }

    fn send_ack(&mut self, id: &str, status: u8, peer: &str, group_id: Option<String>) {
        if !self.is_connected(peer) { return; }
        let d = self.envelope(TYPE_ACK, &AckPayload { message_id: id.to_string(), status, group_id });
        self.send_raw(d, peer, Done::None);
    }

    // Delete -----------------------------------------------------------------

    pub fn delete_messages(&mut self, ids: &[String], conv_id: &str, for_everyone: bool) {
        if ids.is_empty() || !self.conversations.contains_key(conv_id) { return; }
        let set: HashSet<String> = ids.iter().cloned().collect();
        self.remove_messages(&set, conv_id);
        self.persist();
        if for_everyone {
            let c = &self.conversations[conv_id];
            let mut ctl = ControlMessage::new(ControlKind::Delete);
            ctl.ids = Some(ids.to_vec());
            ctl.group_id = if c.is_group() { Some(conv_id.to_string()) } else { None };
            for p in self.recipients(c) { self.send_control(ctl.clone(), &p); }
        }
    }

    pub fn clear_chat(&mut self, conv_id: &str, for_everyone: bool) {
        let Some(c) = self.conversations.get(conv_id) else { return };
        let ids: HashSet<String> = c.messages.iter().map(|m| m.id.clone()).collect();
        self.remove_messages(&ids, conv_id);
        self.conversations.get_mut(conv_id).unwrap().unread_count = 0;
        self.persist();
        if for_everyone {
            let c = &self.conversations[conv_id];
            let mut ctl = ControlMessage::new(ControlKind::Clear);
            ctl.group_id = if c.is_group() { Some(conv_id.to_string()) } else { None };
            for p in self.recipients(c) { self.send_control(ctl.clone(), &p); }
        }
    }

    /// Remove a chat entirely (contact + messages + remembered address). Groups are left first.
    pub fn delete_conversation(&mut self, conv_id: &str) {
        let Some(c) = self.conversations.get(conv_id).cloned() else { return };
        if c.is_group() {
            self.deleted_groups.insert(conv_id.to_string(), c.group.as_ref().map(|g| g.version).unwrap_or(0));
            self.store.save_deleted_groups(&self.deleted_groups);
            self.leave_group(conv_id);
        }
        let ids: HashSet<String> = c.messages.iter().map(|m| m.id.clone()).collect();
        self.remove_messages(&ids, conv_id);
        self.conversations.remove(conv_id);
        if !c.is_group() {
            if let Some(h) = &c.peer.last_address {
                let still = self.conversations.values().any(|x| !x.is_group() && x.peer.last_address.as_ref() == Some(h));
                if !still && self.known_hosts.remove(h) { self.dirty_hosts = true; }
            }
        }
        self.persist();
    }

    // Groups -----------------------------------------------------------------

    pub fn create_group(&mut self, name: &str, members: &[String]) -> String {
        let gid = format!("group-{}", new_uuid());
        let t = name.trim();
        let mut all = vec![self.local_device_id.clone()];
        all.extend(members.iter().filter(|m| **m != self.local_device_id).cloned());
        let mut names = HashMap::new();
        names.insert(self.local_device_id.clone(), self.display_name.clone());
        for m in members {
            names.insert(m.clone(), self.conversations.get(m).map(|c| c.peer.display_name.clone()).unwrap_or_else(|| "Member".into()));
        }
        let info = GroupInfo {
            id: gid.clone(), name: if t.is_empty() { "Group".into() } else { t.into() }, members: all, member_names: names,
            created_by: self.local_device_id.clone(), version: 1, updated_at: now_ref(), avatar_data: None,
        };
        let mut c = Conversation::new(&gid, Peer::new(&gid, &info.name));
        c.group = Some(info.clone());
        self.conversations.insert(gid.clone(), c);
        self.persist();
        self.broadcast_group(&info);
        gid
    }

    pub fn rename_group(&mut self, gid: &str, name: &str) {
        let t = name.trim().to_string();
        if t.is_empty() { return; }
        self.mutate_group(gid, move |g| g.name = t);
    }

    pub fn add_members(&mut self, ids: &[String], gid: &str) {
        let names: Vec<(String, String)> = ids.iter().map(|i| (i.clone(), self.conversations.get(i).map(|c| c.peer.display_name.clone()).unwrap_or_else(|| "Member".into()))).collect();
        self.mutate_group(gid, move |g| {
            for (id, n) in names {
                if !g.members.contains(&id) { g.members.push(id.clone()); g.member_names.insert(id, n); }
            }
        });
    }

    pub fn remove_member(&mut self, id: &str, gid: &str) {
        let Some(before) = self.conversations.get(gid).and_then(|c| c.group.clone()) else { return };
        let rid = id.to_string();
        self.mutate_group(gid, move |g| g.members.retain(|m| *m != rid));
        if before.members.iter().any(|m| m == id) {
            if let Some(g) = self.conversations.get(gid).and_then(|c| c.group.clone()) {
                let mut c = ControlMessage::new(ControlKind::GroupUpdate);
                c.group_id = Some(gid.into());
                c.group = Some(g);
                self.send_control(c, id);
            }
        }
    }

    pub fn leave_group(&mut self, gid: &str) {
        let Some(g) = self.conversations.get(gid).and_then(|c| c.group.clone()) else { return };
        let me0 = self.local_device_id.clone();
        for p in g.members.iter().filter(|m| **m != me0) {
            let mut c = ControlMessage::new(ControlKind::GroupLeave);
            c.group_id = Some(gid.into());
            self.send_control(c, p);
        }
        let me = self.local_device_id.clone();
        if let Some(g) = self.conversations.get_mut(gid).and_then(|c| c.group.as_mut()) { g.members.retain(|m| *m != me); }
        self.persist();
    }

    fn mutate_group(&mut self, gid: &str, change: impl FnOnce(&mut GroupInfo)) {
        let me = self.local_device_id.clone();
        let name = self.display_name.clone();
        let Some(c) = self.conversations.get_mut(gid) else { return };
        let Some(g) = c.group.as_mut() else { return };
        change(g);
        g.member_names.insert(me, name);
        g.version = g.version.max(0) + 1;
        g.updated_at = now_ref();
        c.peer.display_name = g.name.clone();
        let info = g.clone();
        self.persist();
        self.broadcast_group(&info);
    }

    fn broadcast_group(&mut self, g: &GroupInfo) {
        let mut c = ControlMessage::new(ControlKind::GroupUpdate);
        c.group_id = Some(g.id.clone());
        c.group = Some(g.clone());
        let set: HashSet<String> = g.members.iter().cloned().collect();
        for p in set { if p != self.local_device_id { self.send_control(c.clone(), &p); } }
    }

    fn apply_group_update(&mut self, incoming: GroupInfo) {
        if let Some(c) = self.conversations.get_mut(&incoming.id) {
            if let Some(local) = &c.group {
                let newer = incoming.version > local.version || (incoming.version == local.version && incoming.updated_at > local.updated_at);
                if !newer { return; }
                c.peer.display_name = incoming.name.clone();
                c.group = Some(incoming);
                self.persist();
                return;
            }
        }
        if !incoming.members.contains(&self.local_device_id) { return; }
        if let Some(&del) = self.deleted_groups.get(&incoming.id) {
            if incoming.version <= del + 1 { return; }
            self.deleted_groups.remove(&incoming.id);
            self.store.save_deleted_groups(&self.deleted_groups);
        }
        let mut c = Conversation::new(&incoming.id, Peer::new(&incoming.id, &incoming.name));
        c.group = Some(incoming.clone());
        self.conversations.insert(incoming.id.clone(), c);
        self.persist();
    }

    fn ensure_group_placeholder(&mut self, gid: &str, sender: &str) {
        if self.conversations.contains_key(gid) || self.deleted_groups.contains_key(gid) { return; }
        let mut names = HashMap::new();
        names.insert(sender.to_string(), self.conversations.get(sender).map(|c| c.peer.display_name.clone()).unwrap_or_else(|| "Member".into()));
        names.insert(self.local_device_id.clone(), self.display_name.clone());
        let info = GroupInfo {
            id: gid.into(), name: "Group".into(), members: vec![sender.into(), self.local_device_id.clone()], member_names: names,
            created_by: sender.into(), version: 0, updated_at: DISTANT_PAST, avatar_data: None,
        };
        let mut c = Conversation::new(gid, Peer::new(gid, &info.name));
        c.group = Some(info);
        self.conversations.insert(gid.into(), c);
        let mut ctl = ControlMessage::new(ControlKind::GroupInfoRequest);
        ctl.group_id = Some(gid.into());
        self.send_control(ctl, sender);
    }

    // Profile -----------------------------------------------------------------

    pub fn update_display_name(&mut self, name: &str) {
        let t = name.trim();
        if t.is_empty() || t == self.display_name { return; }
        self.display_name = t.to_string();
        self.bump_profile_and_broadcast();
    }

    /// `jpeg` must already be prepared (256 px JPEG); None clears the avatar.
    pub fn update_avatar(&mut self, jpeg: Option<Vec<u8>>) {
        self.avatar_data = jpeg;
        self.bump_profile_and_broadcast();
    }

    fn bump_profile_and_broadcast(&mut self) {
        self.profile_version += 1;
        self.save_profile();
        for p in self.connected_peer_ids() {
            let c = self.profile_control();
            self.send_control(c, &p);
        }
    }

    fn profile_control(&self) -> ControlMessage {
        let mut c = ControlMessage::new(ControlKind::Profile);
        c.name = Some(self.display_name.clone());
        c.avatar = self.avatar_data.clone();
        c.version = Some(self.profile_version);
        c
    }

    // Display helpers ---------------------------------------------------------

    pub fn sorted_conversation_ids(&self) -> Vec<String> {
        let mut v: Vec<&Conversation> = self.conversations.values().collect();
        v.sort_by(|a, b| {
            let da = a.last_message().map(|m| m.timestamp).unwrap_or(a.peer.last_seen);
            let db = b.last_message().map(|m| m.timestamp).unwrap_or(b.peer.last_seen);
            db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
        });
        v.into_iter().map(|c| c.id.clone()).collect()
    }

    pub fn member_name(&self, peer: &str, conv_id: Option<&str>) -> String {
        if peer == self.local_device_id { return self.display_name.clone(); }
        if let Some(c) = self.conversations.get(peer) { return c.peer.display_name.clone(); }
        if let Some(n) = conv_id.and_then(|c| self.conversations.get(c)).and_then(|c| c.group.as_ref()).and_then(|g| g.member_names.get(peer)) {
            return n.clone();
        }
        "Member".into()
    }

    pub fn avatar_for(&self, peer: &str) -> Option<&Vec<u8>> {
        if peer == self.local_device_id { return self.avatar_data.as_ref(); }
        self.conversations.get(peer).and_then(|c| c.peer.avatar_data.as_ref())
    }

    pub fn sender_name(&self, m: &ChatMessage, conv_id: &str) -> String {
        if m.sender_id == self.local_device_id { return self.display_name.clone(); }
        if let Some(c) = self.conversations.get(&m.sender_id) { return c.peer.display_name.clone(); }
        m.sender_name.clone().unwrap_or_else(|| self.member_name(&m.sender_id, Some(conv_id)))
    }

    pub fn is_online(&self, c: &Conversation) -> bool {
        match &c.group {
            Some(g) => g.members.iter().any(|m| *m != self.local_device_id && self.is_connected(m)),
            None => self.is_connected(&c.id),
        }
    }

    pub fn known_peers(&self) -> Vec<Peer> {
        let mut v: Vec<Peer> = self.conversations.values().filter(|c| !c.is_group()).map(|c| c.peer.clone()).collect();
        v.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        v
    }

    pub fn attachment_path(&self, a: &Attachment) -> Option<PathBuf> {
        a.local_file.as_ref().map(|r| self.store.location(r))
    }

    pub fn fingerprint(&self) -> String {
        self.crypto.identity_dh_public().iter().take(8).map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
    }

    pub fn has_pending_for(&self, m: &ChatMessage) -> bool {
        m.pending_recipients.as_ref().map(|p| p.iter().any(|x| !self.is_connected(x))).unwrap_or(false)
    }

    fn ensure_conversation(&mut self, peer: &str, name: &str) {
        if peer == self.local_device_id || peer.starts_with("group-") { return; }
        if !self.conversations.contains_key(peer) {
            let mut p = Peer::new(peer, name);
            p.is_online = true;
            self.conversations.insert(peer.into(), Conversation::new(peer, p));
        }
    }

    pub fn shutdown(&mut self) {
        self.flush();
        for l in self.links.values() { if let Some(s) = &l.stream { let _ = s.shutdown(std::net::Shutdown::Both); } }
    }
}

fn engine_loop(h: CoreHandle, rx: Receiver<Event>) {
    loop {
        let first = rx.recv_timeout(Duration::from_millis(250));
        let mut core = h.lock();
        let got = first.is_ok();
        match first {
            Ok(ev) => {
                core.handle_event(ev);
                for _ in 0..256 {
                    match rx.try_recv() { Ok(ev) => core.handle_event(ev), Err(_) => break }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        core.periodic();
        if got { core.ping_notify(); }
    }
}

//! TCP link plumbing: listener, dialer, one reader + one writer thread per link.
//! All state decisions are made by the engine (chat.rs); threads only move bytes
//! and report events through a channel.
use socket2::{Domain, Protocol, SockAddr, Socket, TcpKeepalive, Type};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub const DEFAULT_TCP_PORT: u16 = 47823;
pub const BEACON_PORT: u16 = 47824;
pub const MAX_PAYLOAD: u32 = 150 * 1024 * 1024;

pub enum Done {
    None,
    Token(u64),
    Chan(Sender<bool>),
}

pub struct WriteJob {
    pub data: Vec<u8>,
    pub done: Done,
}

pub enum Event {
    Incoming(TcpStream),
    Dialed { link: u64, result: Result<TcpStream, String> },
    Frame { link: u64, data: Vec<u8> },
    Closed { link: u64 },
    WriteDone { token: u64, ok: bool },
    TransferProgress { id: String, progress: f32 },
    IncomingDone { id: String, peer: String, ok: bool },
    OutgoingDone { key: String, msg_id: String, conv_id: String, ok: bool },
    Beacon { host: String, remote_id: String },
    Ui, // wake the engine (e.g. after a UI action)
}

#[derive(Clone)]
pub struct LinkWriter {
    pub tx: Sender<WriteJob>,
    pub closed: Arc<AtomicBool>,
}

impl LinkWriter {
    pub fn send(&self, data: Vec<u8>, done: Done) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            report(&done, false, None);
            return false;
        }
        self.tx.send(WriteJob { data, done }).is_ok()
    }
}

fn report(done: &Done, ok: bool, events: Option<&Sender<Event>>) {
    match done {
        Done::None => {}
        Done::Token(t) => {
            if let Some(ev) = events { let _ = ev.send(Event::WriteDone { token: *t, ok }); }
        }
        Done::Chan(c) => { let _ = c.send(ok); }
    }
}

pub fn configure(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
    let sock = socket2::SockRef::from(stream);
    let ka = TcpKeepalive::new().with_time(Duration::from_secs(10)).with_interval(Duration::from_secs(5));
    let _ = sock.set_tcp_keepalive(&ka);
    let _ = stream.set_read_timeout(None);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
}

/// Spawn reader/writer threads for an established stream.
pub fn spawn_link(link: u64, stream: TcpStream, events: Sender<Event>) -> Option<(LinkWriter, TcpStream)> {
    configure(&stream);
    let reader = stream.try_clone().ok()?;
    let mut writer_stream = stream.try_clone().ok()?;
    let closed = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel::<WriteJob>();

    // Reader
    {
        let events = events.clone();
        let closed = closed.clone();
        let mut r = reader;
        thread::Builder::new().name(format!("link-r-{link}")).spawn(move || {
            let mut hdr = [0u8; 4];
            loop {
                if r.read_exact(&mut hdr).is_err() { break; }
                let len = u32::from_be_bytes(hdr);
                if len == 0 || len > MAX_PAYLOAD { break; }
                let mut buf = vec![0u8; len as usize];
                if r.read_exact(&mut buf).is_err() { break; }
                if events.send(Event::Frame { link, data: buf }).is_err() { break; }
            }
            closed.store(true, Ordering::SeqCst);
            let _ = r.shutdown(std::net::Shutdown::Both);
            let _ = events.send(Event::Closed { link });
        }).ok()?;
    }
    // Writer
    {
        let events = events.clone();
        let closed = closed.clone();
        thread::Builder::new().name(format!("link-w-{link}")).spawn(move || {
            let mut failed = false;
            while let Ok(job) = rx.recv() {
                if failed || closed.load(Ordering::SeqCst) {
                    report(&job.done, false, Some(&events));
                    continue;
                }
                let mut frame = Vec::with_capacity(4 + job.data.len());
                frame.extend_from_slice(&(job.data.len() as u32).to_be_bytes());
                frame.extend_from_slice(&job.data);
                let ok = writer_stream.write_all(&frame).is_ok();
                report(&job.done, ok, Some(&events));
                if !ok {
                    failed = true;
                    closed.store(true, Ordering::SeqCst);
                    let _ = writer_stream.shutdown(std::net::Shutdown::Both);
                    let _ = events.send(Event::Closed { link });
                }
            }
        }).ok()?;
    }
    Some((LinkWriter { tx, closed }, stream))
}

/// Listen on `port` (falls back to an ephemeral port if it is taken). Returns the bound port.
pub fn start_listener(port: u16, events: Sender<Event>) -> Option<u16> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
        .or_else(|e| {
            eprintln!("[Net] port {port} unavailable ({e}); using an ephemeral port");
            TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
        })
        .ok()?;
    let bound = listener.local_addr().ok()?.port();
    thread::Builder::new().name("listener".into()).spawn(move || {
        for s in listener.incoming() {
            match s {
                Ok(stream) => { if events.send(Event::Incoming(stream)).is_err() { break; } }
                Err(e) => { eprintln!("[Net] accept error: {e}"); thread::sleep(Duration::from_millis(200)); }
            }
        }
    }).ok()?;
    Some(bound)
}

pub fn dial(link: u64, host: String, port: u16, timeout: Duration, events: Sender<Event>) {
    let _ = thread::Builder::new().name(format!("dial-{link}")).spawn(move || {
        let result = (|| -> Result<TcpStream, String> {
            let addrs: Vec<SocketAddr> = (host.as_str(), port).to_socket_addrs().map_err(|e| e.to_string())?.collect();
            let addr = addrs.iter().find(|a| a.is_ipv4()).or(addrs.first()).ok_or("no address")?;
            let sock = Socket::new(Domain::for_address(*addr), Type::STREAM, Some(Protocol::TCP)).map_err(|e| e.to_string())?;
            sock.connect_timeout(&SockAddr::from(*addr), timeout).map_err(|e| e.to_string())?;
            Ok(sock.into())
        })();
        let _ = events.send(Event::Dialed { link, result });
    });
}

// Interfaces --------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Iface {
    pub name: String,
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
}

pub fn ipv4_interfaces() -> Vec<Iface> {
    let mut out = vec![];
    if let Ok(list) = if_addrs::get_if_addrs() {
        for i in list {
            if let if_addrs::IfAddr::V4(v4) = &i.addr {
                if v4.ip.is_loopback() || v4.ip.is_link_local() || v4.ip.is_unspecified() { continue; }
                out.push(Iface { name: i.name.clone(), ip: v4.ip, netmask: v4.netmask });
            }
        }
    }
    out
}

pub fn own_addresses() -> Vec<String> {
    ipv4_interfaces().iter().map(|i| i.ip.to_string()).collect()
}

/// IPv4 dotted host of a socket address (handles IPv4-mapped IPv6).
pub fn ipv4_host(addr: &SocketAddr) -> Option<String> {
    match addr.ip() {
        IpAddr::V4(v) => Some(v.to_string()),
        IpAddr::V6(v) => v.to_ipv4_mapped().map(|x| x.to_string()),
    }
}

// UDP beacon (best effort) --------------------------------------------------

pub struct Beacon {
    sock: Option<UdpSocket>,
    device_id: String,
}

impl Beacon {
    pub fn start(device_id: &str, events: Sender<Event>) -> Beacon {
        let sock = (|| -> std::io::Result<UdpSocket> {
            let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
            s.set_reuse_address(true)?;
            s.set_broadcast(true)?;
            s.bind(&SockAddr::from(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, BEACON_PORT)))?;
            Ok(s.into())
        })();
        let sock = match sock {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[Beacon] unavailable: {e}");
                return Beacon { sock: None, device_id: device_id.into() };
            }
        };
        if let Ok(r) = sock.try_clone() {
            let _ = thread::Builder::new().name("beacon".into()).spawn(move || {
                let mut buf = [0u8; 512];
                loop {
                    match r.recv_from(&mut buf) {
                        Ok((n, from)) => {
                            if let Ok(text) = std::str::from_utf8(&buf[..n]) {
                                if let Some(id) = text.strip_prefix("SCv2|") {
                                    if let Some(host) = ipv4_host(&from) {
                                        if events.send(Event::Beacon { host, remote_id: id.to_string() }).is_err() { break; }
                                    }
                                }
                            }
                        }
                        Err(_) => thread::sleep(Duration::from_millis(500)),
                    }
                }
            });
        }
        Beacon { sock: Some(sock), device_id: device_id.into() }
    }

    /// Broadcast "SCv2|<deviceId>" so iOS peers dial us back.
    pub fn announce(&self) {
        let Some(s) = &self.sock else { return };
        let msg = format!("SCv2|{}", self.device_id);
        let mut targets = vec![Ipv4Addr::BROADCAST];
        for i in ipv4_interfaces() {
            let ip = u32::from(i.ip);
            let mask = u32::from(i.netmask);
            let b = Ipv4Addr::from((ip & mask) | !mask);
            if !targets.contains(&b) { targets.push(b); }
        }
        for t in targets {
            let _ = s.send_to(msg.as_bytes(), SocketAddrV4::new(t, BEACON_PORT));
        }
    }
}


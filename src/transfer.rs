//! Chunked, encrypted file transfer (64 KB AES-256-GCM chunks, combined box,
//! AAD = transferId(16) ‖ index BE ‖ total BE). Matches Swift FileTransferEngine.
use crate::crypto::{open_combined, seal_combined};
use crate::model::chunk_frame;
use crate::net::{Done, Event, LinkWriter};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const CHUNK_SIZE: usize = 64 * 1024;
pub const MAX_IN_FLIGHT: usize = 8;

pub enum Cmd {
    Register { transfer_id: String, peer: String, key: Vec<u8>, total: u32, chunk_size: usize, path: PathBuf },
    Chunk { peer: String, data: Vec<u8> },
    Cancel(String),
}

struct Incoming {
    id: String,
    key: Vec<u8>,
    total: u32,
    chunk_size: usize,
    file: File,
    received: HashSet<u32>,
    last_progress: Instant,
}

pub type Writers = Arc<Mutex<HashMap<String, LinkWriter>>>;

#[derive(Clone)]
pub struct Transfers {
    tx: Sender<Cmd>,
    cancelled: Arc<Mutex<HashSet<String>>>,
    events: Sender<Event>,
    writers: Writers,
}

impl Transfers {
    pub fn start(events: Sender<Event>, writers: Writers) -> Self {
        let (tx, rx) = channel::<Cmd>();
        let ev = events.clone();
        thread::Builder::new().name("transfer-recv".into()).spawn(move || {
            let mut incoming: HashMap<String, Incoming> = HashMap::new();
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    Cmd::Register { transfer_id, peer, key, total, chunk_size, path } => {
                        let k = format!("{peer}|{transfer_id}");
                        incoming.remove(&k);
                        let file = OpenOptions::new().create(true).write(true).truncate(true).open(&path);
                        if total == 0 { continue; }
                        match file {
                            Ok(file) => {
                                incoming.insert(k, Incoming { id: transfer_id, key, total, chunk_size: chunk_size.max(1), file, received: HashSet::new(), last_progress: Instant::now() - Duration::from_secs(1) });
                            }
                            Err(e) => eprintln!("[FileTransfer] cannot open {}: {e}", path.display()),
                        }
                    }
                    Cmd::Cancel(id) => { incoming.retain(|_, t| t.id != id); }
                    Cmd::Chunk { peer, data } => {
                        let Some((tid, index, total, sealed)) = chunk_frame::decode(&data) else { continue };
                        let id = chunk_frame::uuid_string(&tid);
                        let k = format!("{peer}|{id}");
                        let Some(t) = incoming.get_mut(&k) else { continue };
                        if total != t.total || index >= t.total || t.received.contains(&index) { continue; }
                        let aad = chunk_frame::aad(&tid, index, total);
                        let plain = match open_combined(&t.key, sealed, &aad) {
                            Ok(p) => p,
                            Err(e) => { eprintln!("[FileTransfer] chunk {index} failed: {e}"); continue; }
                        };
                        if t.file.seek(SeekFrom::Start(index as u64 * t.chunk_size as u64)).is_err() || t.file.write_all(&plain).is_err() {
                            eprintln!("[FileTransfer] write failed");
                            continue;
                        }
                        t.received.insert(index);
                        if t.received.len() == t.total as usize {
                            let _ = t.file.flush();
                            let id = t.id.clone();
                            incoming.remove(&k);
                            let _ = ev.send(Event::IncomingDone { id, peer, ok: true });
                        } else if t.last_progress.elapsed() > Duration::from_millis(200) {
                            t.last_progress = Instant::now();
                            let _ = ev.send(Event::TransferProgress { id: t.id.clone(), progress: t.received.len() as f32 / t.total as f32 });
                        }
                    }
                }
            }
        }).expect("transfer thread");
        Transfers { tx, cancelled: Arc::new(Mutex::new(HashSet::new())), events, writers }
    }

    pub fn register(&self, transfer_id: &str, peer: &str, key: Vec<u8>, total: u32, chunk_size: usize, path: PathBuf) {
        let _ = self.tx.send(Cmd::Register { transfer_id: transfer_id.into(), peer: peer.into(), key, total, chunk_size, path });
    }

    pub fn chunk(&self, peer: &str, data: Vec<u8>) {
        let _ = self.tx.send(Cmd::Chunk { peer: peer.into(), data });
    }

    pub fn cancel(&self, id: &str) {
        self.cancelled.lock().unwrap().insert(id.into());
        let _ = self.tx.send(Cmd::Cancel(id.into()));
    }

    fn is_cancelled(&self, id: &str) -> bool { self.cancelled.lock().unwrap().contains(id) }

    /// Stream a file to a peer in a background thread. Completion is reported as Event::OutgoingDone.
    pub fn send(&self, path: PathBuf, transfer_id: String, key: Vec<u8>, chunk_count: u32, peer: String, done_key: String, conv_id: String) {
        self.cancelled.lock().unwrap().remove(&transfer_id);
        let me = self.clone();
        thread::Builder::new().name("transfer-send".into()).spawn(move || {
            let ok = me.send_blocking(&path, &transfer_id, &key, chunk_count, &peer);
            let _ = me.events.send(Event::OutgoingDone { key: done_key, msg_id: transfer_id, conv_id, ok });
        }).ok();
    }

    fn send_blocking(&self, path: &PathBuf, transfer_id: &str, key: &[u8], chunk_count: u32, peer: &str) -> bool {
        if chunk_count == 0 { return true; }
        let Some(tid) = chunk_frame::uuid_bytes(transfer_id) else { return false };
        let Ok(mut f) = File::open(path) else { return false };
        let (done_tx, done_rx) = channel::<bool>();
        let mut in_flight = 0usize;
        let mut failed = false;
        let mut last = Instant::now() - Duration::from_secs(1);
        let mut buf = vec![0u8; CHUNK_SIZE];
        for i in 0..chunk_count {
            if failed || self.is_cancelled(transfer_id) { break; }
            // read up to CHUNK_SIZE
            let mut n = 0;
            while n < CHUNK_SIZE {
                match f.read(&mut buf[n..]) { Ok(0) => break, Ok(k) => n += k, Err(_) => { failed = true; break; } }
            }
            if failed || n == 0 { failed = true; break; }
            let aad = chunk_frame::aad(&tid, i, chunk_count);
            let Ok(sealed) = seal_combined(key, &buf[..n], &aad) else { failed = true; break };
            let frame = chunk_frame::encode(&tid, i, chunk_count, &sealed);
            while in_flight >= MAX_IN_FLIGHT {
                match done_rx.recv_timeout(Duration::from_secs(60)) {
                    Ok(ok) => { in_flight -= 1; if !ok { failed = true; } }
                    Err(_) => { failed = true; in_flight = 0; }
                }
            }
            if failed { break; }
            let writer = self.writers.lock().unwrap().get(peer).cloned();
            match writer {
                Some(w) => {
                    if w.send(frame, Done::Chan(done_tx.clone())) { in_flight += 1; } else { failed = true; }
                }
                None => { failed = true; }
            }
            if last.elapsed() > Duration::from_millis(200) {
                last = Instant::now();
                let _ = self.events.send(Event::TransferProgress { id: transfer_id.into(), progress: (i + 1) as f32 / chunk_count as f32 });
            }
        }
        while in_flight > 0 {
            match done_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(ok) => { in_flight -= 1; if !ok { failed = true; } }
                Err(_) => { failed = true; break; }
            }
        }
        !failed && !self.is_cancelled(transfer_id)
    }
}

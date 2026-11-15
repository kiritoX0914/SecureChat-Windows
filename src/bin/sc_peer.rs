//! Headless interop peer. Runs one role of the multi-process protocol test
//! (see interop/README.md). Roles: S (normally the Swift app), R1, R2.
//!   sc-peer <role> <workdir>
//! Env: SC_TEST_FILE / SC_TEST_SHA (file to transfer + its sha256).
use securechat_core::chat::{Config, Core, CoreHandle};
use securechat_core::model::*;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const S: &str = "B0000000-0000-0000-0000-00000000000B";
const R1: &str = "A0000000-0000-0000-0000-00000000000A";
const R2: &str = "C0000000-0000-0000-0000-00000000000C";

struct Runner {
    role: String,
    h: CoreHandle,
    start: Instant,
    pass: u32,
    fail: u32,
}

impl Runner {
    fn log(&self, s: &str) {
        println!("[{} {:6.2}] {s}", self.role, self.start.elapsed().as_secs_f64());
    }
    fn check(&mut self, name: &str, ok: bool) {
        if ok { self.pass += 1 } else { self.fail += 1 }
        self.log(&format!("{} {name}", if ok { "PASS" } else { "FAIL" }));
    }
    fn wait_until(&self, t: f64) {
        let target = self.start + Duration::from_secs_f64(t);
        let now = Instant::now();
        if target > now { std::thread::sleep(target - now); }
    }
    fn find(&self, conv: &str, text: &str) -> Option<ChatMessage> {
        let c = self.h.lock();
        c.conversations.get(conv).and_then(|c| c.messages.iter().find(|m| m.text == text).cloned())
    }
    fn group_id(&self) -> Option<String> {
        let c = self.h.lock();
        c.conversations.values().find(|c| c.is_group()).map(|c| c.id.clone())
    }
    fn incoming_of_type(&self, conv: &str, from: &str, ct: u8) -> Option<ChatMessage> {
        let c = self.h.lock();
        c.conversations.get(conv).and_then(|c| c.messages.iter().find(|m| m.content_type == ct && m.sender_id == from).cloned())
    }
    fn file_sha(&self, m: &ChatMessage) -> Option<(String, usize)> {
        let c = self.h.lock();
        let p = c.attachment_path(m.attachment.as_ref()?)?;
        let data = std::fs::read(p).ok()?;
        Some((hex(&Sha256::digest(&data)), data.len()))
    }
    fn dump(&self) {
        let c = self.h.lock();
        for conv in c.conversations.values() {
            self.log(&format!("conv {} name={} online={} group={} msgs={}", &conv.id[..8.min(conv.id.len())], conv.peer.display_name, c.is_online(conv), conv.group.as_ref().map(|g| g.name.as_str()).unwrap_or("-"), conv.messages.len()));
            for m in &conv.messages {
                self.log(&format!("   msg from={} status={} text={} pending={:?} complete={:?}", &m.sender_id[..1], m.status, m.text.chars().take(40).collect::<String>(), m.pending_recipients.as_ref().map(|p| p.iter().map(|x| x[..1].to_string()).collect::<Vec<_>>()), m.attachment.as_ref().and_then(|a| a.is_complete)));
            }
        }
    }
    fn finish(&self, t: f64) -> ! {
        self.wait_until(t);
        self.dump();
        self.log(&format!("RESULT {} passes={} failures={}", if self.fail == 0 { "PASS" } else { "FAIL" }, self.pass, self.fail));
        self.h.lock().flush();
        std::process::exit(if self.fail == 0 { 0 } else { 1 });
    }
}

fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }

fn test_png() -> Vec<u8> {
    let mut img = image::RgbaImage::new(300, 200);
    for (x, y, p) in img.enumerate_pixels_mut() { *p = image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]); }
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let role = args.get(1).cloned().unwrap_or_else(|| "R1".into());
    let dir = PathBuf::from(args.get(2).cloned().unwrap_or_else(|| "/tmp".into()));
    let file = std::env::var("SC_TEST_FILE").unwrap_or_default();
    let sha = std::env::var("SC_TEST_SHA").unwrap_or_default();
    let (id, port, name) = match role.as_str() {
        "S" => (S, 47823, "User-S"),
        "R1" => (R1, 47901, "User-R1"),
        _ => (R2, 47902, "User-R2"),
    };
    let data = dir.join(format!("home-{role}"));
    let _ = std::fs::create_dir_all(&data);
    let h = Core::start(Config { data_dir: Some(data), port, device_id: Some(id.into()), display_name: Some(name.into()), dial_known_on_start: false, enable_beacon: false });
    let mut r = Runner { role: role.clone(), h: h.clone(), start: Instant::now(), pass: 0, fail: 0 };
    r.log(&format!("started as {id} port {:?}", h.lock().listening_port));

    match role.as_str() {
        // Same script as interop/InteropMain.swift role S (used for Rust-only runs).
        "S" => {
            r.wait_until(8.0);
            h.lock().send_message("hello from S", R1, None);
            h.lock().send_message("hello from S", R2, None);
            r.wait_until(10.0);
            h.lock().send_file(std::path::Path::new(&file), R2, None);
            r.wait_until(12.0);
            r.check("S got text from R1", r.find(R1, "hello from R1").is_some());
            r.check("S got text from R2", r.find(R2, "hello from R2").is_some());
            r.check("S text delivered to R1", r.find(R1, "hello from S").map(|m| m.status >= STATUS_DELIVERED).unwrap_or(false));
            if let Some(m) = r.find(R2, "hello from R2") { h.lock().send_message("S replies to R2", R2, Some(&m.id)); }
            r.wait_until(14.0);
            h.lock().update_display_name("S Renamed");
            r.wait_until(16.0);
            h.lock().mark_as_read(R1);
            r.wait_until(18.0);
            if let Some(m) = r.find(R2, "hello from S") { h.lock().delete_messages(&[m.id], R2, true); }
            r.wait_until(24.0);
            r.check("S: R1's deleted-for-both message is gone", r.find(R1, "hello from R1").is_none());
            let mine = r.find(R1, "hello from S");
            let rep = r.find(R1, "R1 replies");
            r.check("S got reply with replyToId", rep.is_some() && mine.is_some() && rep.unwrap().reply_to_id == mine.map(|m| m.id));
            let f = r.incoming_of_type(R1, R1, CONTENT_FILE);
            r.check("S file from R1 complete", f.as_ref().and_then(|m| m.attachment.as_ref()).and_then(|a| a.is_complete) == Some(true));
            r.check("S file sha256 matches", f.and_then(|m| r.file_sha(&m)).map(|(s, _)| s == sha).unwrap_or(false));
            let img = r.incoming_of_type(R1, R1, CONTENT_IMAGE);
            r.check("S got image from R1", img.and_then(|m| r.file_sha(&m)).map(|(_, n)| n > 0).unwrap_or(false));
            {
                let c = h.lock();
                let p = c.conversations.get(R1).map(|c| c.peer.clone());
                r.check("S sees R1's new name", p.as_ref().map(|p| p.display_name == "R1 Renamed").unwrap_or(false));
                r.check("S sees R1's avatar", p.and_then(|p| p.avatar_data).map(|a| a.len() > 100).unwrap_or(false));
            }
            r.wait_until(25.0);
            h.lock().create_group("Interop Group", &[R1.to_string(), R2.to_string()]);
            r.wait_until(29.0);
            if let Some(g) = r.group_id() { h.lock().send_message("group hi from S", &g, None); }
            r.wait_until(36.0);
            let g = r.group_id().unwrap_or_default();
            r.check("S got group msg from R1", r.find(&g, "group hi from R1").is_some());
            r.check("S got group msg from R2", r.find(&g, "group hi from R2").is_some());
            r.check("S group msg delivered to all", r.find(&g, "group hi from S").map(|m| m.pending_recipients.is_none()).unwrap_or(false));
            r.wait_until(40.0);
            r.check("S sees group renamed by R2", h.lock().conversations.get(&g).and_then(|c| c.group.as_ref()).map(|g| g.name == "Renamed Group").unwrap_or(false));
            r.wait_until(41.0);
            h.lock().clear_chat(R1, true);
            r.finish(46.0);
        }
        "R1" => {
            r.wait_until(2.0);
            h.lock().connect_by_ip("127.0.0.1", 47823);
            r.wait_until(8.0);
            h.lock().send_message("hello from R1", S, None);
            r.wait_until(10.0);
            h.lock().send_file(std::path::Path::new(&file), S, None);
            r.wait_until(12.0);
            r.check("R1 got text from S", r.find(S, "hello from S").is_some());
            r.check("R1 text delivered to S", r.find(S, "hello from R1").map(|m| m.status >= STATUS_DELIVERED).unwrap_or(false));
            if let Some(m) = r.find(S, "hello from S") { h.lock().send_message("R1 replies", S, Some(&m.id)); }
            r.wait_until(13.0);
            h.lock().send_image_bytes(&test_png(), "test.png", S, None);
            r.wait_until(14.0);
            h.lock().update_display_name("R1 Renamed");
            let av = securechat_core::media::prepare_avatar(&test_png()).ok();
            h.lock().update_avatar(av);
            r.wait_until(17.0);
            r.check("R1 sees read receipt from S", r.find(S, "hello from R1").map(|m| m.status == STATUS_READ).unwrap_or(false));
            r.check("R1 file to S delivered", r.incoming_of_type(S, R1, CONTENT_FILE).map(|m| m.status >= STATUS_DELIVERED).unwrap_or(false));
            r.check("R1 image to S delivered", r.incoming_of_type(S, R1, CONTENT_IMAGE).map(|m| m.status >= STATUS_DELIVERED).unwrap_or(false));
            r.wait_until(18.0);
            if let Some(m) = r.find(S, "hello from R1") { h.lock().delete_messages(&[m.id], S, true); }
            r.wait_until(24.0);
            r.check("R1 sees S's new name", h.lock().conversations.get(S).map(|c| c.peer.display_name == "S Renamed").unwrap_or(false));
            r.check("R1 got message from R2 (Rust<->Rust)", r.find(R2, "r2 to r1").is_some());
            r.wait_until(28.0);
            match r.group_id() {
                Some(g) => h.lock().send_message("group hi from R1", &g, None),
                None => r.check("R1 has group", false),
            }
            r.wait_until(36.0);
            let g = r.group_id().unwrap_or_default();
            r.check("R1 group name synced", h.lock().conversations.get(&g).and_then(|c| c.group.as_ref()).map(|x| x.name == "Interop Group").unwrap_or(false));
            r.check("R1 got group msg from S", r.find(&g, "group hi from S").is_some());
            r.check("R1 got group msg from R2", r.find(&g, "group hi from R2").is_some());
            r.check("R1 group msg delivered to all", r.find(&g, "group hi from R1").map(|m| m.pending_recipients.is_none()).unwrap_or(false));
            r.wait_until(40.0);
            r.check("R1 sees group renamed by R2", h.lock().conversations.get(&g).and_then(|c| c.group.as_ref()).map(|g| g.name == "Renamed Group").unwrap_or(false));
            r.wait_until(44.0);
            r.check("R1 chat with S cleared by S (clear for both)", h.lock().conversations.get(S).map(|c| c.messages.is_empty()).unwrap_or(false));
            r.finish(46.0);
        }
        _ => {
            r.wait_until(3.0);
            h.lock().connect_by_ip("127.0.0.1", 47823);
            r.wait_until(4.0);
            h.lock().connect_by_ip("127.0.0.1", 47901);
            r.wait_until(8.0);
            h.lock().send_message("hello from R2", S, None);
            h.lock().send_message("r2 to r1", R1, None);
            r.wait_until(12.0);
            r.check("R2 got text from S", r.find(S, "hello from S").is_some());
            r.check("R2 text delivered to S", r.find(S, "hello from R2").map(|m| m.status >= STATUS_DELIVERED).unwrap_or(false));
            r.wait_until(24.0);
            let f = r.incoming_of_type(S, S, CONTENT_FILE);
            r.check("R2 file from S complete", f.as_ref().and_then(|m| m.attachment.as_ref()).and_then(|a| a.is_complete) == Some(true));
            r.check("R2 file sha256 matches", f.and_then(|m| r.file_sha(&m)).map(|(s, _)| s == sha).unwrap_or(false));
            let mine = r.find(S, "hello from R2");
            let rep = r.find(S, "S replies to R2");
            r.check("R2 got reply with replyToId", rep.is_some() && mine.is_some() && rep.unwrap().reply_to_id == mine.map(|m| m.id));
            r.check("R2: S's deleted-for-both message is gone", r.find(S, "hello from S").is_none());
            r.check("R2 sees S's new name", h.lock().conversations.get(S).map(|c| c.peer.display_name == "S Renamed").unwrap_or(false));
            r.check("R2 sees R1's new name", h.lock().conversations.get(R1).map(|c| c.peer.display_name == "R1 Renamed").unwrap_or(false));
            r.wait_until(30.0);
            match r.group_id() {
                Some(g) => h.lock().send_message("group hi from R2", &g, None),
                None => r.check("R2 has group", false),
            }
            r.wait_until(36.0);
            let g = r.group_id().unwrap_or_default();
            r.check("R2 group name synced", h.lock().conversations.get(&g).and_then(|c| c.group.as_ref()).map(|x| x.name == "Interop Group").unwrap_or(false));
            r.check("R2 got group msg from S", r.find(&g, "group hi from S").is_some());
            r.check("R2 got group msg from R1", r.find(&g, "group hi from R1").is_some());
            r.wait_until(37.0);
            h.lock().rename_group(&g, "Renamed Group");
            r.finish(46.0);
        }
    }
}

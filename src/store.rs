//! Local persistence under %APPDATA%\SecureChat (override with SC_DATA_DIR).
use crate::crypto::{KeyFile, SessionStore};
use crate::model::Conversation;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub device_id: String,
    pub display_name: String,
    #[serde(default, with = "crate::model::b64opt")]
    pub avatar: Option<Vec<u8>>,
    #[serde(default)]
    pub profile_version: i64,
}

pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(dir: Option<PathBuf>) -> Self {
        let dir = dir
            .or_else(|| std::env::var_os("SC_DATA_DIR").map(PathBuf::from))
            .unwrap_or_else(|| dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("SecureChat"));
        let _ = fs::create_dir_all(dir.join("attachments"));
        Store { dir }
    }

    fn path(&self, name: &str) -> PathBuf { self.dir.join(name) }

    fn load<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        let data = fs::read(self.path(name)).ok()?;
        match serde_json::from_slice(&data) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("[Store] {name} unreadable: {e}");
                let _ = fs::copy(self.path(name), self.path(&format!("{name}.corrupt")));
                None
            }
        }
    }

    fn save<T: Serialize>(&self, name: &str, v: &T) {
        let Ok(data) = serde_json::to_vec(v) else { return };
        let tmp = self.path(&format!("{name}.tmp"));
        if fs::write(&tmp, &data).is_ok() {
            if fs::rename(&tmp, self.path(name)).is_err() {
                let _ = fs::write(self.path(name), &data);
                let _ = fs::remove_file(&tmp);
            }
        }
    }

    pub fn load_profile(&self) -> Option<Profile> { self.load("profile.json") }
    pub fn save_profile(&self, p: &Profile) { self.save("profile.json", p) }
    pub fn load_keys(&self) -> Option<KeyFile> { self.load("keys.json") }
    pub fn save_keys(&self, k: &KeyFile) { self.save("keys.json", k) }
    pub fn load_sessions(&self) -> SessionStore { self.load("sessions.json").unwrap_or_default() }
    pub fn save_sessions(&self, s: &SessionStore) { self.save("sessions.json", s) }
    pub fn load_conversations(&self) -> HashMap<String, Conversation> { self.load("conversations.json").unwrap_or_default() }
    pub fn save_conversations(&self, c: &HashMap<String, Conversation>) { self.save("conversations.json", c) }
    pub fn load_known_hosts(&self) -> Vec<String> { self.load("known_hosts.json").unwrap_or_default() }
    pub fn save_known_hosts(&self, h: &[String]) { self.save("known_hosts.json", &h) }
    pub fn load_deleted_groups(&self) -> HashMap<String, i64> { self.load("deleted_groups.json").unwrap_or_default() }
    pub fn save_deleted_groups(&self, g: &HashMap<String, i64>) { self.save("deleted_groups.json", g) }

    // Attachments ----------------------------------------------------------

    pub fn attachments_dir(&self) -> PathBuf { self.dir.join("attachments") }

    pub fn location(&self, rel: &str) -> PathBuf {
        let mut p = self.attachments_dir();
        for part in rel.split(['/', '\\']) {
            if part.is_empty() || part == "." || part == ".." { continue; }
            p.push(part);
        }
        p
    }

    /// Returns (relative path "<id>/<name>", absolute path) and creates the folder.
    pub fn prepare(&self, message_id: &str, filename: &str) -> (String, PathBuf) {
        let name = sanitize_filename(filename);
        let rel = format!("{}/{}", sanitize_filename(message_id), name);
        let abs = self.location(&rel);
        if let Some(parent) = abs.parent() { let _ = fs::create_dir_all(parent); }
        (rel, abs)
    }

    pub fn write(&self, data: &[u8], message_id: &str, filename: &str) -> Option<String> {
        let (rel, abs) = self.prepare(message_id, filename);
        fs::write(&abs, data).ok().map(|_| rel)
    }

    pub fn copy_in(&self, src: &Path, message_id: &str, filename: &str) -> Option<String> {
        let (rel, abs) = self.prepare(message_id, filename);
        fs::copy(src, &abs).ok().map(|_| rel)
    }

    pub fn delete(&self, message_id: &str) {
        let p = self.attachments_dir().join(sanitize_filename(message_id));
        let _ = fs::remove_dir_all(p);
    }
}

/// Make a peer-supplied file name safe on Windows (no traversal, reserved names, etc.).
pub fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = base
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    s = s.trim().trim_end_matches(['.', ' ']).to_string();
    if s.is_empty() || s == "." || s == ".." { s = "file".into(); }
    let stem = s.split('.').next().unwrap_or("").to_uppercase();
    const RESERVED: [&str; 22] = ["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"];
    if RESERVED.contains(&stem.as_str()) { s = format!("_{s}"); }
    if s.chars().count() > 150 {
        let ext: String = s.rsplit_once('.').map(|(_, e)| e.chars().take(10).collect()).unwrap_or_default();
        let head: String = s.chars().take(120).collect();
        s = if ext.is_empty() { head } else { format!("{head}.{ext}") };
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sanitize() {
        assert_eq!(sanitize_filename("../../evil.exe"), "evil.exe");
        assert_eq!(sanitize_filename("a:b?.txt"), "a_b_.txt");
        assert_eq!(sanitize_filename("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_filename(".."), "file");
    }
}

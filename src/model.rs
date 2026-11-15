//! Wire / persistence types. Field names, enum raw values and encodings match
//! the Swift `Codable` types exactly:
//!  * `Data`  -> standard padded base64 string
//!  * `Date`  -> JSON number, seconds since 2001-01-01T00:00:00Z
//!  * `UUID`  -> uppercase hyphenated string
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Seconds between 1970-01-01 and 2001-01-01 (Foundation reference date).
pub const REFERENCE_EPOCH_OFFSET: f64 = 978_307_200.0;
/// Foundation `Date.distantPast` as a reference-date interval.
pub const DISTANT_PAST: f64 = -63_114_076_800.0;

pub fn now_ref() -> f64 {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    d.as_secs_f64() - REFERENCE_EPOCH_OFFSET
}

pub fn now_unix() -> f64 {
    now_ref() + REFERENCE_EPOCH_OFFSET
}

pub fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string().to_uppercase()
}

pub fn norm_uuid(s: &str) -> String {
    s.to_uppercase()
}

pub mod b64 {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s.trim()).map_err(serde::de::Error::custom)
    }
}

pub mod b64opt {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(b) => s.serialize_str(&STANDARD.encode(b)),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        match s {
            Some(s) => STANDARD.decode(s.trim()).map(Some).map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

/// UUID strings are normalised to upper case when decoded.
pub mod uuid_up {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &String, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_uppercase())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
        let s = String::deserialize(d)?;
        uuid::Uuid::parse_str(&s).map_err(serde::de::Error::custom)?;
        Ok(s.to_uppercase())
    }
}
pub mod uuid_up_opt {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &Option<String>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(v) => s.serialize_str(&v.to_uppercase()),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        match s {
            Some(s) => {
                uuid::Uuid::parse_str(&s).map_err(serde::de::Error::custom)?;
                Ok(Some(s.to_uppercase()))
            }
            None => Ok(None),
        }
    }
}
pub mod uuid_up_vec_opt {
    use serde::{Deserialize, Deserializer, Serializer, Serialize};
    pub fn serialize<S: Serializer>(v: &Option<Vec<String>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(v) => v.iter().map(|x| x.to_uppercase()).collect::<Vec<_>>().serialize(s),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<String>>, D::Error> {
        let s: Option<Vec<String>> = Option::deserialize(d)?;
        match s {
            Some(v) => {
                let mut out = Vec::with_capacity(v.len());
                for x in v {
                    uuid::Uuid::parse_str(&x).map_err(serde::de::Error::custom)?;
                    out.push(x.to_uppercase());
                }
                Ok(Some(out))
            }
            None => Ok(None),
        }
    }
}

fn default_unknown() -> String { "Unknown".into() }
fn default_status() -> u8 { STATUS_SENT }
fn default_file() -> String { "file".into() }
fn default_mime() -> String { "application/octet-stream".into() }

// MARK: Peer --------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub id: String,
    #[serde(default = "default_unknown")]
    pub display_name: String,
    #[serde(default)]
    pub is_online: bool,
    #[serde(default = "now_ref")]
    pub last_seen: f64,
    #[serde(default, with = "b64opt", skip_serializing_if = "Option::is_none", rename = "identityDHKeyData")]
    pub identity_dh_key_data: Option<Vec<u8>>,
    #[serde(default, with = "b64opt", skip_serializing_if = "Option::is_none")]
    pub identity_signing_key_data: Option<Vec<u8>>,
    #[serde(default, with = "b64opt", skip_serializing_if = "Option::is_none")]
    pub avatar_data: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_address: Option<String>,
}

impl Peer {
    pub fn new(id: &str, name: &str) -> Self {
        Peer {
            id: id.to_string(),
            display_name: name.to_string(),
            is_online: false,
            last_seen: now_ref(),
            identity_dh_key_data: None,
            identity_signing_key_data: None,
            avatar_data: None,
            last_address: None,
        }
    }
    /// Same short fingerprint as the Swift app: first 8 bytes of the DH key, upper hex.
    pub fn fingerprint(&self) -> String {
        match &self.identity_dh_key_data {
            Some(k) => k.iter().take(8).map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "),
            None => "Unknown".into(),
        }
    }
}

// MARK: ChatMessage -------------------------------------------------------

pub const STATUS_SENDING: u8 = 0;
pub const STATUS_SENT: u8 = 1;
pub const STATUS_DELIVERED: u8 = 2;
pub const STATUS_READ: u8 = 3;
pub const STATUS_FAILED: u8 = 4;

pub const CONTENT_TEXT: u8 = 0;
pub const CONTENT_IMAGE: u8 = 1;
pub const CONTENT_FILE: u8 = 2;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    #[serde(with = "uuid_up")]
    pub id: String,
    pub sender_id: String,
    pub recipient_id: String,
    #[serde(default)]
    pub text: String,
    pub timestamp: f64,
    #[serde(default = "default_status")]
    pub status: u8,
    #[serde(default)]
    pub content_type: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<Attachment>,
    #[serde(default, with = "uuid_up_opt", skip_serializing_if = "Option::is_none")]
    pub reply_to_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_sender: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_recipients: Option<Vec<String>>,
}

impl ChatMessage {
    pub fn is_video(&self) -> bool {
        self.content_type == CONTENT_FILE && self.attachment.as_ref().map(|a| a.is_video()).unwrap_or(false)
    }
    pub fn preview_text(&self) -> String {
        match self.content_type {
            CONTENT_IMAGE => "📷 Photo".into(),
            CONTENT_FILE => {
                if self.is_video() {
                    "🎬 Video".into()
                } else {
                    format!("📎 {}", self.attachment.as_ref().map(|a| a.filename.as_str()).unwrap_or("File"))
                }
            }
            _ => self.text.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    #[serde(default = "default_file")]
    pub filename: String,
    #[serde(default = "default_mime")]
    pub mime_type: String,
    #[serde(default, with = "b64opt", skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<u8>>,
    #[serde(default)]
    pub file_size: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<i64>,
    #[serde(default, with = "b64opt", skip_serializing_if = "Option::is_none")]
    pub thumbnail_data: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_file: Option<String>,
    #[serde(default, with = "uuid_up_opt", skip_serializing_if = "Option::is_none")]
    pub transfer_id: Option<String>,
    #[serde(default, with = "b64opt", skip_serializing_if = "Option::is_none")]
    pub file_key: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_complete: Option<bool>,
}

impl Attachment {
    pub fn is_video(&self) -> bool { self.mime_type.to_lowercase().starts_with("video/") }
    pub fn is_image(&self) -> bool { self.mime_type.to_lowercase().starts_with("image/") }
    pub fn is_available(&self) -> bool {
        self.data.is_some() || (self.local_file.is_some() && self.is_complete.unwrap_or(true))
    }
}

pub fn format_size(n: i64) -> String {
    let n = n.max(0) as f64;
    if n < 1000.0 { format!("{} bytes", n as i64) }
    else if n < 1e6 { format!("{:.0} KB", n / 1e3) }
    else if n < 1e9 { format!("{:.1} MB", n / 1e6) }
    else { format!("{:.2} GB", n / 1e9) }
}

// MARK: Group -----------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GroupInfo {
    pub id: String,
    pub name: String,
    pub members: Vec<String>,
    #[serde(default)]
    pub member_names: HashMap<String, String>,
    pub created_by: String,
    pub version: i64,
    pub updated_at: f64,
    #[serde(default, with = "b64opt", skip_serializing_if = "Option::is_none")]
    pub avatar_data: Option<Vec<u8>>,
}

// MARK: Conversation (local persistence only) --------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub peer: Peer,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub unread_count: i64,
    #[serde(skip)]
    pub is_typing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_key_bundle: Option<PreKeyBundlePayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupInfo>,
}

impl Conversation {
    pub fn new(id: &str, peer: Peer) -> Self {
        Conversation { id: id.into(), peer, messages: vec![], unread_count: 0, is_typing: false, pre_key_bundle: None, group: None }
    }
    pub fn is_group(&self) -> bool { self.group.is_some() }
    pub fn last_message(&self) -> Option<&ChatMessage> { self.messages.last() }
}

// MARK: Wire protocol ----------------------------------------------------

pub const TYPE_HELLO: u8 = 0;
pub const TYPE_PREKEY_MESSAGE: u8 = 1;
pub const TYPE_MESSAGE: u8 = 2;
pub const TYPE_ACK: u8 = 3;
pub const TYPE_TYPING: u8 = 4;
pub const TYPE_PING: u8 = 7;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireEnvelope {
    #[serde(rename = "type")]
    pub kind: u8,
    pub sender_id: String,
    pub sender_name: String,
    #[serde(with = "b64")]
    pub payload: Vec<u8>,
}

impl WireEnvelope {
    pub fn make<T: Serialize>(kind: u8, sender_id: &str, sender_name: &str, payload: &T) -> Vec<u8> {
        let env = WireEnvelope {
            kind,
            sender_id: sender_id.into(),
            sender_name: sender_name.into(),
            payload: serde_json::to_vec(payload).unwrap_or_default(),
        };
        serde_json::to_vec(&env).unwrap_or_default()
    }
    pub fn raw(kind: u8, sender_id: &str, sender_name: &str, payload: Vec<u8>) -> Vec<u8> {
        let env = WireEnvelope { kind, sender_id: sender_id.into(), sender_name: sender_name.into(), payload };
        serde_json::to_vec(&env).unwrap_or_default()
    }
    pub fn known_type(&self) -> bool {
        matches!(self.kind, TYPE_HELLO | TYPE_PREKEY_MESSAGE | TYPE_MESSAGE | TYPE_ACK | TYPE_TYPING | TYPE_PING)
    }
}

/// Binary file chunk frame: [0x01][16 B transferId][4 B index BE][4 B total BE][AES-GCM combined box]
pub mod chunk_frame {
    pub const MAGIC: u8 = 0x01;
    pub const HEADER_LEN: usize = 1 + 16 + 4 + 4;

    pub fn uuid_bytes(id: &str) -> Option<[u8; 16]> {
        uuid::Uuid::parse_str(id).ok().map(|u| *u.as_bytes())
    }

    pub fn encode(transfer: &[u8; 16], index: u32, total: u32, sealed: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + sealed.len());
        out.push(MAGIC);
        out.extend_from_slice(transfer);
        out.extend_from_slice(&index.to_be_bytes());
        out.extend_from_slice(&total.to_be_bytes());
        out.extend_from_slice(sealed);
        out
    }

    pub fn decode(data: &[u8]) -> Option<([u8; 16], u32, u32, &[u8])> {
        if data.len() <= HEADER_LEN || data[0] != MAGIC { return None; }
        let mut id = [0u8; 16];
        id.copy_from_slice(&data[1..17]);
        let index = u32::from_be_bytes([data[17], data[18], data[19], data[20]]);
        let total = u32::from_be_bytes([data[21], data[22], data[23], data[24]]);
        Some((id, index, total, &data[HEADER_LEN..]))
    }

    pub fn aad(transfer: &[u8; 16], index: u32, total: u32) -> Vec<u8> {
        let mut d = Vec::with_capacity(24);
        d.extend_from_slice(transfer);
        d.extend_from_slice(&index.to_be_bytes());
        d.extend_from_slice(&total.to_be_bytes());
        d
    }

    pub fn uuid_string(b: &[u8; 16]) -> String {
        uuid::Uuid::from_bytes(*b).to_string().to_uppercase()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ControlKind {
    SessionInit,
    Delete,
    Clear,
    Profile,
    GroupUpdate,
    GroupLeave,
    GroupInfoRequest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlMessage {
    pub kind: ControlKind,
    #[serde(default, with = "uuid_up_vec_opt", skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, with = "b64opt", skip_serializing_if = "Option::is_none")]
    pub avatar: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
}

impl ControlMessage {
    pub fn new(kind: ControlKind) -> Self {
        ControlMessage { kind, ids: None, group_id: None, name: None, avatar: None, group: None, version: None }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InnerEnvelope {
    pub v: i64,
    pub control: ControlMessage,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreKeyBundlePayload {
    #[serde(with = "b64", rename = "identityDHKey")]
    pub identity_dh_key: Vec<u8>,
    #[serde(with = "b64")]
    pub identity_signing_key: Vec<u8>,
    pub signed_pre_key_id: u32,
    #[serde(with = "b64")]
    pub signed_pre_key: Vec<u8>,
    #[serde(with = "b64")]
    pub signed_pre_key_signature: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub one_time_pre_key_id: Option<u32>,
    #[serde(default, with = "b64opt", skip_serializing_if = "Option::is_none")]
    pub one_time_pre_key: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_session: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EncryptedMessagePayload {
    #[serde(with = "uuid_up")]
    pub message_id: String,
    #[serde(with = "b64")]
    pub ciphertext: Vec<u8>,
    #[serde(with = "b64")]
    pub nonce: Vec<u8>,
    #[serde(with = "b64")]
    pub ratchet_public_key: Vec<u8>,
    pub previous_chain_length: u32,
    pub message_number: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreKeyMessagePayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub one_time_pre_key_id: Option<u32>,
    pub signed_pre_key_id: u32,
    #[serde(with = "b64", rename = "senderIdentityDHKey")]
    pub sender_identity_dh_key: Vec<u8>,
    #[serde(with = "b64")]
    pub sender_identity_signing_key: Vec<u8>,
    #[serde(with = "b64")]
    pub ephemeral_key: Vec<u8>,
    pub encrypted_message: EncryptedMessagePayload,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AckPayload {
    #[serde(with = "uuid_up")]
    pub message_id: String,
    pub status: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypingPayload {
    pub is_typing: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PingPayload {
    pub t: f64,
}

//! Signal-style X3DH + Double Ratchet, byte-for-byte compatible with the
//! Swift `SignalCrypto.swift` / `CryptoEngine.swift` (CryptoKit):
//!  * X25519 (Curve25519.KeyAgreement) raw 32-byte keys
//!  * Ed25519 (Curve25519.Signing) 32-byte seed / public key, 64-byte signature
//!  * X3DH: HKDF-SHA256(ikm = DH1‖DH2‖DH3[‖DH4], salt = 32×0x00, info = "X3DH", 32 B)
//!  * Root KDF: HKDF-SHA256(ikm = DH, salt = rootKey, info = "DoubleRatchet", 64 B) -> root ‖ chain
//!  * Chain step: mk = HMAC-SHA256(ck, 0x01), ck' = HMAC-SHA256(ck, 0x02)
//!  * Message keys: HKDF-SHA256(ikm = mk, salt = 32×0x00, info = "MessageKeys", 44 B) -> key(32) ‖ nonce(12)
//!  * AES-256-GCM, no AAD, ciphertext field = ct ‖ tag(16)
use crate::model::{EncryptedMessagePayload, PreKeyBundlePayload, PreKeyMessagePayload};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::{HashMap, HashSet};
use x25519_dalek::{PublicKey as XPublic, StaticSecret};

#[derive(Debug, Clone, PartialEq)]
pub enum CryptoError {
    NoSession,
    InvalidSignature,
    DecryptionFailed,
    InvalidKey,
    SessionNotFound,
    PreKeyNotFound,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
impl std::error::Error for CryptoError {}

pub fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b
}

fn arr32(d: &[u8]) -> Result<[u8; 32], CryptoError> {
    d.try_into().map_err(|_| CryptoError::InvalidKey)
}

pub fn x25519_public(private: &[u8; 32]) -> [u8; 32] {
    XPublic::from(&StaticSecret::from(*private)).to_bytes()
}

pub fn x25519(private: &[u8; 32], public: &[u8]) -> Result<[u8; 32], CryptoError> {
    let p = XPublic::from(arr32(public)?);
    Ok(StaticSecret::from(*private).diffie_hellman(&p).to_bytes())
}

pub fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut out = vec![0u8; len];
    hk.expand(info, &mut out).expect("hkdf length");
    out
}

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("hmac key");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

pub fn aes_gcm_seal(key: &[u8], nonce: &[u8], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let c = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::InvalidKey)?;
    c.encrypt(Nonce::from_slice(nonce), Payload { msg: plaintext, aad }).map_err(|_| CryptoError::DecryptionFailed)
}

pub fn aes_gcm_open(key: &[u8], nonce: &[u8], ct_and_tag: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if nonce.len() != 12 || ct_and_tag.len() < 16 { return Err(CryptoError::DecryptionFailed); }
    let c = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::InvalidKey)?;
    c.decrypt(Nonce::from_slice(nonce), Payload { msg: ct_and_tag, aad }).map_err(|_| CryptoError::DecryptionFailed)
}

/// CryptoKit `AES.GCM.SealedBox.combined` = nonce(12) ‖ ciphertext ‖ tag(16), random nonce.
pub fn seal_combined(key: &[u8], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ct = aes_gcm_seal(key, &nonce, plaintext, aad)?;
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn open_combined(key: &[u8], combined: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if combined.len() < 28 { return Err(CryptoError::DecryptionFailed); }
    aes_gcm_open(key, &combined[..12], &combined[12..], aad)
}

pub fn ed25519_public(seed: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(seed).verifying_key().to_bytes()
}

pub fn ed25519_sign(seed: &[u8; 32], msg: &[u8]) -> Vec<u8> {
    SigningKey::from_bytes(seed).sign(msg).to_bytes().to_vec()
}

pub fn ed25519_verify(public: &[u8], sig: &[u8], msg: &[u8]) -> bool {
    let Ok(pk) = arr32(public) else { return false };
    let Ok(vk) = VerifyingKey::from_bytes(&pk) else { return false };
    let Ok(sig): Result<[u8; 64], _> = sig.try_into() else { return false };
    vk.verify(msg, &ed25519_dalek::Signature::from_bytes(&sig)).is_ok()
}

// MARK: Keys ----------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub struct IdentityKeyPair {
    #[serde(with = "crate::model::b64")]
    pub dh_private: Vec<u8>,
    #[serde(with = "crate::model::b64")]
    pub signing_private: Vec<u8>,
}

impl IdentityKeyPair {
    pub fn generate() -> Self {
        IdentityKeyPair { dh_private: random32().to_vec(), signing_private: random32().to_vec() }
    }
    pub fn dh_priv(&self) -> [u8; 32] { arr32(&self.dh_private).expect("dh key") }
    pub fn sign_priv(&self) -> [u8; 32] { arr32(&self.signing_private).expect("sign key") }
    pub fn dh_public(&self) -> [u8; 32] { x25519_public(&self.dh_priv()) }
    pub fn signing_public(&self) -> [u8; 32] { ed25519_public(&self.sign_priv()) }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SignedPreKey {
    pub id: u32,
    #[serde(with = "crate::model::b64")]
    pub private: Vec<u8>,
    #[serde(with = "crate::model::b64")]
    pub signature: Vec<u8>,
}

impl SignedPreKey {
    pub fn generate(id: u32, identity: &IdentityKeyPair) -> Self {
        let private = random32();
        let public = x25519_public(&private);
        SignedPreKey { id, private: private.to_vec(), signature: ed25519_sign(&identity.sign_priv(), &public) }
    }
    pub fn priv_key(&self) -> [u8; 32] { arr32(&self.private).expect("spk") }
    pub fn public(&self) -> [u8; 32] { x25519_public(&self.priv_key()) }
}

// MARK: X3DH ----------------------------------------------------------------

pub struct X3DHInitiatorResult {
    pub shared_secret: Vec<u8>,
    pub ephemeral_public: [u8; 32],
}

pub fn x3dh_initiate(
    our: &IdentityKeyPair,
    their_identity_dh: &[u8],
    their_signing: &[u8],
    their_spk: &[u8],
    their_spk_sig: &[u8],
    their_otpk: Option<&[u8]>,
    ephemeral_private: Option<[u8; 32]>,
) -> Result<X3DHInitiatorResult, CryptoError> {
    if !ed25519_verify(their_signing, their_spk_sig, their_spk) {
        return Err(CryptoError::InvalidSignature);
    }
    let eph = ephemeral_private.unwrap_or_else(random32);
    let dh1 = x25519(&our.dh_priv(), their_spk)?;
    let dh2 = x25519(&eph, their_identity_dh)?;
    let dh3 = x25519(&eph, their_spk)?;
    let mut cat = Vec::with_capacity(128);
    cat.extend_from_slice(&dh1);
    cat.extend_from_slice(&dh2);
    cat.extend_from_slice(&dh3);
    if let Some(o) = their_otpk {
        cat.extend_from_slice(&x25519(&eph, o)?);
    }
    Ok(X3DHInitiatorResult {
        shared_secret: hkdf_sha256(&cat, &[0u8; 32], b"X3DH", 32),
        ephemeral_public: x25519_public(&eph),
    })
}

pub fn x3dh_respond(
    our: &IdentityKeyPair,
    our_spk: &[u8; 32],
    our_otpk: Option<&[u8; 32]>,
    their_identity_dh: &[u8],
    their_ephemeral: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let dh1 = x25519(our_spk, their_identity_dh)?;
    let dh2 = x25519(&our.dh_priv(), their_ephemeral)?;
    let dh3 = x25519(our_spk, their_ephemeral)?;
    let mut cat = Vec::with_capacity(128);
    cat.extend_from_slice(&dh1);
    cat.extend_from_slice(&dh2);
    cat.extend_from_slice(&dh3);
    if let Some(o) = our_otpk {
        cat.extend_from_slice(&x25519(o, their_ephemeral)?);
    }
    Ok(hkdf_sha256(&cat, &[0u8; 32], b"X3DH", 32))
}

// MARK: Double Ratchet --------------------------------------------------------

const MAX_SKIPPED_KEYS: usize = 100;

fn root_kdf(root: &[u8], dh: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let out = hkdf_sha256(dh, root, b"DoubleRatchet", 64);
    (out[..32].to_vec(), out[32..].to_vec())
}

fn chain_step(ck: &[u8]) -> (Vec<u8>, Vec<u8>) {
    (hmac_sha256(ck, &[0x01]), hmac_sha256(ck, &[0x02]))
}

pub fn message_material(mk: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let out = hkdf_sha256(mk, &[0u8; 32], b"MessageKeys", 44);
    (out[..32].to_vec(), out[32..].to_vec())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DoubleRatchetSession {
    #[serde(default, with = "crate::model::b64opt")]
    dh_sending_private: Option<Vec<u8>>,
    #[serde(default, with = "crate::model::b64opt")]
    dh_receiving: Option<Vec<u8>>,
    #[serde(with = "crate::model::b64")]
    root_key: Vec<u8>,
    #[serde(default, with = "crate::model::b64opt")]
    sending_chain: Option<Vec<u8>>,
    #[serde(default, with = "crate::model::b64opt")]
    receiving_chain: Option<Vec<u8>>,
    pub send_n: u32,
    pub recv_n: u32,
    pub prev_send_len: u32,
    #[serde(default)]
    skipped: HashMap<String, String>, // "<hex pub>:<n>" -> hex mk
}

impl DoubleRatchetSession {
    pub fn new_initiator(shared: &[u8], their_spk: &[u8], dh_private: Option<[u8; 32]>) -> Result<Self, CryptoError> {
        let new_dh = dh_private.unwrap_or_else(random32);
        let dh_out = x25519(&new_dh, their_spk)?;
        let (root, ck) = root_kdf(shared, &dh_out);
        Ok(DoubleRatchetSession {
            dh_sending_private: Some(new_dh.to_vec()),
            dh_receiving: Some(their_spk.to_vec()),
            root_key: root,
            sending_chain: Some(ck),
            receiving_chain: None,
            send_n: 0,
            recv_n: 0,
            prev_send_len: 0,
            skipped: HashMap::new(),
        })
    }

    pub fn new_responder(shared: &[u8], our_spk: &[u8; 32]) -> Self {
        DoubleRatchetSession {
            dh_sending_private: Some(our_spk.to_vec()),
            dh_receiving: None,
            root_key: shared.to_vec(),
            sending_chain: None,
            receiving_chain: None,
            send_n: 0,
            recv_n: 0,
            prev_send_len: 0,
            skipped: HashMap::new(),
        }
    }

    fn sending_public(&self) -> Option<[u8; 32]> {
        self.dh_sending_private.as_ref().and_then(|d| arr32(d).ok()).map(|p| x25519_public(&p))
    }

    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<EncryptedMessagePayload, CryptoError> {
        let (Some(ck), Some(pubk)) = (self.sending_chain.clone(), self.sending_public()) else {
            return Err(CryptoError::NoSession);
        };
        let (mk, next) = chain_step(&ck);
        self.sending_chain = Some(next);
        let (key, nonce) = message_material(&mk);
        let ct = aes_gcm_seal(&key, &nonce, plaintext, &[])?;
        let n = self.send_n;
        self.send_n += 1;
        Ok(EncryptedMessagePayload {
            message_id: crate::model::new_uuid(),
            ciphertext: ct,
            nonce,
            ratchet_public_key: pubk.to_vec(),
            previous_chain_length: self.prev_send_len,
            message_number: n,
        })
    }

    /// Decrypt; on error `self` may be partially modified, so callers work on a clone.
    pub fn decrypt(&mut self, m: &EncryptedMessagePayload) -> Result<Vec<u8>, CryptoError> {
        // 1. skipped keys
        let idx = format!("{}:{}", hex(&m.ratchet_public_key), m.message_number);
        if let Some(mk_hex) = self.skipped.remove(&idx) {
            let mk = unhex(&mk_hex);
            return decrypt_with(&mk, m);
        }
        // 2. DH ratchet
        if self.dh_receiving.as_deref() != Some(m.ratchet_public_key.as_slice()) {
            self.dh_ratchet(&m.ratchet_public_key, m.previous_chain_length)?;
        }
        // 3. advance
        self.skip_until(m.message_number)?;
        let Some(ck) = self.receiving_chain.clone() else { return Err(CryptoError::DecryptionFailed) };
        let (mk, next) = chain_step(&ck);
        self.receiving_chain = Some(next);
        self.recv_n += 1;
        decrypt_with(&mk, m)
    }

    fn dh_ratchet(&mut self, new_key: &[u8], prev_len: u32) -> Result<(), CryptoError> {
        if self.receiving_chain.is_some() {
            self.skip_until(prev_len)?;
        }
        self.prev_send_len = self.send_n;
        self.send_n = 0;
        self.recv_n = 0;
        self.dh_receiving = Some(new_key.to_vec());
        let their = arr32(new_key)?;
        if let Some(sp) = self.dh_sending_private.as_ref().and_then(|d| arr32(d).ok()) {
            let dh = x25519(&sp, &their)?;
            let (r, c) = root_kdf(&self.root_key, &dh);
            self.root_key = r;
            self.receiving_chain = Some(c);
        }
        let new_dh = random32();
        self.dh_sending_private = Some(new_dh.to_vec());
        let dh2 = x25519(&new_dh, &their)?;
        let (r, c) = root_kdf(&self.root_key, &dh2);
        self.root_key = r;
        self.sending_chain = Some(c);
        Ok(())
    }

    fn skip_until(&mut self, target: u32) -> Result<(), CryptoError> {
        let Some(recv_key) = self.dh_receiving.clone() else { return Ok(()) };
        if target < self.recv_n { return Ok(()); }
        let to_skip = (target - self.recv_n) as usize;
        if self.skipped.len() + to_skip > MAX_SKIPPED_KEYS { return Err(CryptoError::DecryptionFailed); }
        let Some(mut ck) = self.receiving_chain.clone() else { return Ok(()) };
        let kh = hex(&recv_key);
        for n in self.recv_n..target {
            let (mk, next) = chain_step(&ck);
            self.skipped.insert(format!("{kh}:{n}"), hex(&mk));
            ck = next;
        }
        self.receiving_chain = Some(ck);
        self.recv_n = target;
        Ok(())
    }
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).filter_map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()).collect()
}

fn decrypt_with(mk: &[u8], m: &EncryptedMessagePayload) -> Result<Vec<u8>, CryptoError> {
    let (key, nonce) = message_material(mk);
    if m.ciphertext.len() < 16 { return Err(CryptoError::DecryptionFailed); }
    aes_gcm_open(&key, &nonce, &m.ciphertext, &[])
}

// MARK: CryptoEngine ------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct SessionStore {
    pub sessions: HashMap<String, DoubleRatchetSession>,
    pub pre_key_sessions: HashSet<String>,
    #[serde(default)]
    pub ephemeral_keys: HashMap<String, String>, // base64
    #[serde(default)]
    pub used_spk_ids: HashMap<String, u32>,
    #[serde(default)]
    pub used_otpk_ids: HashMap<String, Option<u32>>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct KeyFile {
    pub identity: IdentityKeyPair,
    pub signed_pre_key: SignedPreKey,
}

pub struct CryptoEngine {
    pub identity: IdentityKeyPair,
    pub spk: SignedPreKey,
    pub st: SessionStore,
    pub dirty: bool,
}

use base64::{engine::general_purpose::STANDARD as B64, Engine};

impl CryptoEngine {
    pub fn new(keys: KeyFile, st: SessionStore) -> Self {
        CryptoEngine { identity: keys.identity, spk: keys.signed_pre_key, st, dirty: false }
    }

    pub fn generate_keys() -> KeyFile {
        let identity = IdentityKeyPair::generate();
        let spk = SignedPreKey::generate(1, &identity);
        KeyFile { identity, signed_pre_key: spk }
    }

    pub fn identity_dh_public(&self) -> [u8; 32] { self.identity.dh_public() }

    /// Bundles never advertise one-time pre-keys (same as Swift v1.2+).
    pub fn pre_key_bundle(&self) -> PreKeyBundlePayload {
        PreKeyBundlePayload {
            identity_dh_key: self.identity.dh_public().to_vec(),
            identity_signing_key: self.identity.signing_public().to_vec(),
            signed_pre_key_id: self.spk.id,
            signed_pre_key: self.spk.public().to_vec(),
            signed_pre_key_signature: self.spk.signature.clone(),
            one_time_pre_key_id: None,
            one_time_pre_key: None,
            has_session: None,
            protocol_version: None,
        }
    }

    pub fn reset_session(&mut self, peer: &str) {
        self.st.sessions.remove(peer);
        self.st.pre_key_sessions.remove(peer);
        self.st.ephemeral_keys.remove(peer);
        self.st.used_spk_ids.remove(peer);
        self.st.used_otpk_ids.remove(peer);
        self.dirty = true;
    }

    pub fn has_session(&self, peer: &str) -> bool { self.st.sessions.contains_key(peer) }

    pub fn process_received_bundle(&mut self, b: &PreKeyBundlePayload, peer: &str) -> Result<(), CryptoError> {
        let r = x3dh_initiate(
            &self.identity,
            &b.identity_dh_key,
            &b.identity_signing_key,
            &b.signed_pre_key,
            &b.signed_pre_key_signature,
            b.one_time_pre_key.as_deref(),
            None,
        )?;
        let s = DoubleRatchetSession::new_initiator(&r.shared_secret, &b.signed_pre_key, None)?;
        self.st.sessions.insert(peer.into(), s);
        self.st.pre_key_sessions.insert(peer.into());
        self.st.ephemeral_keys.insert(peer.into(), B64.encode(r.ephemeral_public));
        self.st.used_spk_ids.insert(peer.into(), b.signed_pre_key_id);
        self.st.used_otpk_ids.insert(peer.into(), b.one_time_pre_key_id);
        self.dirty = true;
        Ok(())
    }

    /// Returns (isPreKey, JSON payload bytes).
    pub fn encrypt(&mut self, plaintext: &[u8], peer: &str) -> Result<(bool, Vec<u8>), CryptoError> {
        let Some(session) = self.st.sessions.get_mut(peer) else { return Err(CryptoError::NoSession) };
        let enc = session.encrypt(plaintext)?;
        self.dirty = true;
        if self.st.pre_key_sessions.contains(peer) {
            let eph = self.st.ephemeral_keys.get(peer).and_then(|e| B64.decode(e).ok()).unwrap_or_default();
            let msg = PreKeyMessagePayload {
                one_time_pre_key_id: self.st.used_otpk_ids.get(peer).cloned().flatten(),
                signed_pre_key_id: self.st.used_spk_ids.get(peer).cloned().unwrap_or(self.spk.id),
                sender_identity_dh_key: self.identity.dh_public().to_vec(),
                sender_identity_signing_key: self.identity.signing_public().to_vec(),
                ephemeral_key: eph,
                encrypted_message: enc,
            };
            self.st.pre_key_sessions.remove(peer);
            self.st.ephemeral_keys.remove(peer);
            self.st.used_spk_ids.remove(peer);
            self.st.used_otpk_ids.remove(peer);
            Ok((true, serde_json::to_vec(&msg).unwrap_or_default()))
        } else {
            Ok((false, serde_json::to_vec(&enc).unwrap_or_default()))
        }
    }

    pub fn decrypt_pre_key_message(&mut self, m: &PreKeyMessagePayload, peer: &str) -> Result<Vec<u8>, CryptoError> {
        if m.signed_pre_key_id != self.spk.id { return Err(CryptoError::PreKeyNotFound); }
        if m.one_time_pre_key_id.is_some() {
            // We never publish one-time pre-keys.
            return Err(CryptoError::PreKeyNotFound);
        }
        let ss = x3dh_respond(&self.identity, &self.spk.priv_key(), None, &m.sender_identity_dh_key, &m.ephemeral_key)?;
        let mut s = DoubleRatchetSession::new_responder(&ss, &self.spk.priv_key());
        let pt = s.decrypt(&m.encrypted_message)?;
        self.st.sessions.insert(peer.into(), s);
        self.dirty = true;
        Ok(pt)
    }

    pub fn decrypt_message(&mut self, m: &EncryptedMessagePayload, peer: &str) -> Result<Vec<u8>, CryptoError> {
        let Some(s) = self.st.sessions.get(peer) else { return Err(CryptoError::SessionNotFound) };
        let mut copy = s.clone();
        let pt = copy.decrypt(m)?;
        self.st.sessions.insert(peer.into(), copy);
        self.dirty = true;
        Ok(pt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_handshake_roundtrip() {
        let mut a = CryptoEngine::new(CryptoEngine::generate_keys(), SessionStore::default());
        let mut b = CryptoEngine::new(CryptoEngine::generate_keys(), SessionStore::default());
        let bundle = b.pre_key_bundle();
        a.process_received_bundle(&bundle, "B").unwrap();
        let (pk, p) = a.encrypt(b"hi", "B").unwrap();
        assert!(pk);
        let m: PreKeyMessagePayload = serde_json::from_slice(&p).unwrap();
        assert_eq!(b.decrypt_pre_key_message(&m, "A").unwrap(), b"hi");
        for i in 0..5 {
            let (pk, p) = b.encrypt(format!("b{i}").as_bytes(), "A").unwrap();
            assert!(!pk);
            let m: EncryptedMessagePayload = serde_json::from_slice(&p).unwrap();
            assert_eq!(a.decrypt_message(&m, "B").unwrap(), format!("b{i}").as_bytes());
            let (_, p) = a.encrypt(format!("a{i}").as_bytes(), "B").unwrap();
            let m: EncryptedMessagePayload = serde_json::from_slice(&p).unwrap();
            assert_eq!(b.decrypt_message(&m, "A").unwrap(), format!("a{i}").as_bytes());
        }
        // out of order
        let (_, p1) = a.encrypt(b"x1", "B").unwrap();
        let (_, p2) = a.encrypt(b"x2", "B").unwrap();
        let m2: EncryptedMessagePayload = serde_json::from_slice(&p2).unwrap();
        let m1: EncryptedMessagePayload = serde_json::from_slice(&p1).unwrap();
        assert_eq!(b.decrypt_message(&m2, "A").unwrap(), b"x2");
        assert_eq!(b.decrypt_message(&m1, "A").unwrap(), b"x1");
    }
}

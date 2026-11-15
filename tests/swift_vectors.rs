//! Decrypt frames produced by the Swift (CryptoKit) implementation, playing
//! device B with the private keys published in PROTOCOL-vectors.json.
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use securechat_core::crypto::*;
use securechat_core::model::*;
use sha2::{Digest, Sha256};

fn vectors() -> serde_json::Value {
    serde_json::from_str(include_str!("vectors/PROTOCOL-vectors.json")).unwrap()
}

fn b64(v: &serde_json::Value) -> Vec<u8> { B64.decode(v.as_str().unwrap()).unwrap() }

fn engine_b(v: &serde_json::Value) -> CryptoEngine {
    let b = &v["B"];
    let identity = IdentityKeyPair { dh_private: b64(&b["identityDHPrivate"]), signing_private: b64(&b["identitySigningPrivate"]) };
    let spk_priv = b64(&b["signedPreKeyPrivate"]);
    // Signature taken from B's (Swift) hello frame.
    let hello: WireEnvelope = serde_json::from_slice(&b64(&b["helloFrame"])).unwrap();
    let bundle: PreKeyBundlePayload = serde_json::from_slice(&hello.payload).unwrap();
    let spk = SignedPreKey { id: b["signedPreKeyId"].as_u64().unwrap() as u32, private: spk_priv, signature: bundle.signed_pre_key_signature.clone() };
    CryptoEngine::new(KeyFile { identity, signed_pre_key: spk }, SessionStore::default())
}

#[test]
fn swift_hello_matches_rust_key_derivation_and_signature() {
    let v = vectors();
    let e = engine_b(&v);
    let hello: WireEnvelope = serde_json::from_slice(&b64(&v["B"]["helloFrame"])).unwrap();
    assert_eq!(hello.kind, TYPE_HELLO);
    assert_eq!(hello.sender_id, v["B"]["deviceId"].as_str().unwrap());
    let bundle: PreKeyBundlePayload = serde_json::from_slice(&hello.payload).unwrap();
    let ours = e.pre_key_bundle();
    assert_eq!(bundle.identity_dh_key, ours.identity_dh_key, "X25519 public derivation");
    assert_eq!(bundle.identity_signing_key, ours.identity_signing_key, "Ed25519 public derivation");
    assert_eq!(bundle.signed_pre_key, ours.signed_pre_key, "SPK public derivation");
    assert!(ed25519_verify(&bundle.identity_signing_key, &bundle.signed_pre_key_signature, &bundle.signed_pre_key), "CryptoKit signature verifies");
    assert_eq!(bundle.protocol_version, Some(2));
    // Rust signature over the same key must verify too (Ed25519 is standard; CryptoKit adds randomness only).
    let sig = ed25519_sign(&e.identity.sign_priv(), &bundle.signed_pre_key);
    assert!(ed25519_verify(&bundle.identity_signing_key, &sig, &bundle.signed_pre_key));
}

#[test]
fn decrypt_swift_ratchet_messages() {
    let v = vectors();
    let mut e = engine_b(&v);
    let a_id = v["A"]["deviceId"].as_str().unwrap();
    let frames = v["aToB"].as_array().unwrap();
    let plains = v["aToBPlaintexts"].as_array().unwrap();
    assert_eq!(frames.len(), plains.len());
    for (f, p) in frames.iter().zip(plains) {
        let env: WireEnvelope = serde_json::from_slice(&b64(f)).unwrap();
        assert_eq!(env.sender_id, a_id);
        let pt = match env.kind {
            TYPE_PREKEY_MESSAGE => e.decrypt_pre_key_message(&serde_json::from_slice(&env.payload).unwrap(), a_id).unwrap(),
            TYPE_MESSAGE => e.decrypt_message(&serde_json::from_slice(&env.payload).unwrap(), a_id).unwrap(),
            k => panic!("unexpected type {k}"),
        };
        assert_eq!(String::from_utf8(pt.clone()).unwrap(), p.as_str().unwrap());
        // The plaintext must also parse with our model types.
        if serde_json::from_slice::<ChatMessage>(&pt).is_err() {
            let _inner: InnerEnvelope = serde_json::from_slice(&pt).expect("control or chat message");
        }
    }
    // And B can answer on the same session (A's side is CryptoKit; at least it must encrypt).
    let (pre, _) = e.encrypt(b"{}", a_id).unwrap();
    assert!(!pre);
}

#[test]
fn decrypt_swift_file_chunks() {
    let v = vectors();
    let f = &v["file"];
    let key = b64(&f["key"]);
    let tid = chunk_frame::uuid_bytes(f["transferId"].as_str().unwrap()).unwrap();
    let mut out = vec![0u8; f["size"].as_u64().unwrap() as usize];
    for c in f["chunkFrames"].as_array().unwrap() {
        let data = b64(c);
        let (id, index, total, sealed) = chunk_frame::decode(&data).unwrap();
        assert_eq!(id, tid);
        let plain = open_combined(&key, sealed, &chunk_frame::aad(&id, index, total)).unwrap();
        let off = index as usize * 65536;
        out[off..off + plain.len()].copy_from_slice(&plain);
    }
    let sha: String = Sha256::digest(&out).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(sha, f["sha256"].as_str().unwrap());
}

//! SecureChat core: wire-compatible Rust port of the iOS / macOS SecureChat
//! protocol (X3DH + Double Ratchet over length-framed JSON on TCP 47823).
pub mod model;
pub mod crypto;
pub mod store;
pub mod net;
pub mod transfer;
pub mod media;
pub mod chat;

pub use chat::{Core, CoreHandle, Config};

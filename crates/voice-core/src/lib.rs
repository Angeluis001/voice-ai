//! Shared core for the Voice AI: configuration, session files, client packs,
//! xAI clients, audio helpers and the call engine.
pub mod analysis;
pub mod audio;
pub mod call;
pub mod config;
pub mod esl;
pub mod intent;
pub mod originate;
pub mod pack;
pub mod session;
pub mod slots;
pub mod util;
pub mod xai;

/// A loosely-typed JSON object. Session documents are kept as JSON maps so that
/// fields written by other tools (console write-backs, ingest) survive a round trip.
pub type Doc = serde_json::Map<String, serde_json::Value>;

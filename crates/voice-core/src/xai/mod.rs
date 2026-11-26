//! xAI clients: REST (chat, speech-to-text, text-to-speech, voices) and realtime voice.
pub mod realtime;
pub mod rest;

pub use rest::{XaiError, XaiRest};

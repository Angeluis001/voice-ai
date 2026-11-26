//! Reads the voice agent's session files and turns them into what the console shows:
//! live call sessions, post-call summaries, content packs and device uploads.
pub mod analysis;
pub mod models;
pub mod prompts;
mod pyjson;
pub mod service;
pub mod settings;

pub use analysis::{
    conversation_for_analysis, generate_call_content, heuristic_post_analysis,
    heuristic_roleplay_scorecard, is_collections_session, is_roleplay_session,
    parse_roleplay_scorecard, post_analysis_from_model,
};
pub use service::{
    dedupe_session_turns, parse_iso, sessions_fingerprint, ServiceError, SessionService,
};

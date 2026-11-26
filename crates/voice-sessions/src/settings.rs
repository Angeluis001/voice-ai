//! Console settings from the environment (same variable names as the Python console).
use std::{env, path::PathBuf};

fn var(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[derive(Debug, Clone)]
pub struct ConsoleSettings {
    /// Uploads, device results and (by default) the brand store live here.
    pub artifacts_dir: PathBuf,
    /// `*-session.json` and per-turn result JSON written by the voice agent.
    pub telephony_results_dir: PathBuf,
    /// Call recordings (`<uuid>-call.wav`).
    pub telephony_media_dir: PathBuf,
    /// A call counts as live if it was updated within this many seconds.
    pub active_window_seconds: i64,
    pub brand_dir: PathBuf,
    pub clients_dir: PathBuf,
    pub default_client_id: String,
    /// URL prefix when served behind a reverse proxy (for example `/ops`).
    pub base_path: String,
}

impl ConsoleSettings {
    pub fn from_env() -> Self {
        let artifacts_dir: PathBuf = var("VOICE_IN_ARTIFACTS_DIR")
            .unwrap_or_else(|| "artifacts/voice_in_agent".into())
            .into();
        let brand_dir = var("VOICE_IN_BRAND_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| artifacts_dir.join("brand"));
        Self {
            telephony_results_dir: var("VOICE_IN_TELEPHONY_RESULTS_DIR")
                .unwrap_or_else(|| "docker/sip-stack/runtime/pilot/results".into())
                .into(),
            telephony_media_dir: var("VOICE_IN_TELEPHONY_MEDIA_DIR")
                .unwrap_or_else(|| "docker/sip-stack/runtime/pilot/media".into())
                .into(),
            active_window_seconds: var("VOICE_IN_ACTIVE_WINDOW_SECONDS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(180),
            clients_dir: var("VOICE_AI_CLIENTS_DIR")
                .unwrap_or_else(|| "docker/sip-stack/clients".into())
                .into(),
            default_client_id: var("VOICE_AI_DEFAULT_CLIENT_ID")
                .unwrap_or_else(|| "default".into()),
            base_path: var("VOICE_IN_BASE_PATH")
                .map(|p| p.trim_end_matches('/').to_string())
                .unwrap_or_default(),
            brand_dir,
            artifacts_dir,
        }
    }

    /// Settings rooted in one directory, for tests.
    pub fn for_dir(root: &std::path::Path) -> Self {
        Self {
            artifacts_dir: root.join("artifacts"),
            telephony_results_dir: root.join("results"),
            telephony_media_dir: root.join("media"),
            active_window_seconds: 180,
            brand_dir: root.join("artifacts").join("brand"),
            clients_dir: root.join("clients"),
            default_client_id: "default".into(),
            base_path: String::new(),
        }
    }
}

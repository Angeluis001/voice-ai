//! Environment configuration. Existing variable names are kept so the same
//! `.env` files work for the Rust binaries.
use std::{env, path::PathBuf};

fn var(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn var_or(name: &str, default: &str) -> String {
    var(name).unwrap_or_else(|| default.to_string())
}

fn num<T: std::str::FromStr>(name: &str, default: T) -> T {
    var(name).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Load `.env` from the working directory or any parent. A missing file is fine.
pub fn load_dotenv() {
    let _ = dotenvy::dotenv();
}

#[derive(Debug, Clone)]
pub struct XaiSettings {
    pub api_key: String,
    pub base_url: String,
    pub realtime_url: String,
    pub grok_model: String,
    pub tts_voice: String,
    pub language: String,
    pub stt_keyterm: Option<String>,
}

impl XaiSettings {
    pub fn from_env() -> Self {
        Self {
            api_key: var_or("XAI_API_KEY", ""),
            base_url: var_or("XAI_BASE_URL", "https://api.x.ai/v1")
                .trim_end_matches('/')
                .to_string(),
            realtime_url: var_or(
                "XAI_REALTIME_URL",
                "wss://api.x.ai/v1/realtime?model=grok-voice-think-fast-1.0",
            ),
            grok_model: var_or("VOICE_MVP_GROK_MODEL", "grok-4.3"),
            tts_voice: var_or("VOICE_MVP_TTS_VOICE", "eve"),
            language: var_or("VOICE_MVP_LANGUAGE", "en"),
            stt_keyterm: var("VOICE_MVP_STT_KEYTERM"),
        }
    }
}

/// Turn detection and transcription settings sent in `session.update`.
#[derive(Debug, Clone)]
pub struct VadSettings {
    pub threshold: f64,
    pub silence_ms: u32,
    pub prefix_ms: u32,
    pub transcribe_model: String,
    pub language_hint: String,
}

impl Default for VadSettings {
    fn default() -> Self {
        Self {
            threshold: 0.55,
            silence_ms: 480,
            prefix_ms: 200,
            transcribe_model: "grok-transcribe".into(),
            language_hint: "en".into(),
        }
    }
}

impl VadSettings {
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            threshold: num("VOICE_AI_VAD_THRESHOLD", d.threshold),
            silence_ms: num("VOICE_AI_VAD_SILENCE_MS", d.silence_ms),
            prefix_ms: num("VOICE_AI_VAD_PREFIX_MS", d.prefix_ms),
            transcribe_model: var_or("VOICE_AI_TRANSCRIBE_MODEL", &d.transcribe_model),
            language_hint: var_or("VOICE_AI_LANGUAGE_HINT", &d.language_hint),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EslSettings {
    pub host: String,
    pub port: u16,
    pub password: String,
}

impl EslSettings {
    pub fn from_env() -> Self {
        Self {
            host: var_or("FREESWITCH_HOST", "freeswitch"),
            port: num("FREESWITCH_PORT", 8021),
            password: var("FREESWITCH_SECRET")
                .or_else(|| var("FREESWITCH_ESL_PASSWORD"))
                .unwrap_or_default(),
        }
    }
}

/// Settings for the realtime voice agent.
#[derive(Debug, Clone)]
pub struct AgentSettings {
    pub xai: XaiSettings,
    pub vad: VadSettings,
    pub esl: EslSettings,
    pub listen_port: u16,
    pub result_dir: PathBuf,
    pub media_dir: PathBuf,
    /// PCM rate for softphone calls (FreeSWITCH audio fork).
    pub sample_rate: u32,
    /// PCM rate for the local microphone mode.
    pub local_sample_rate: u32,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub clients_dir: PathBuf,
    pub default_client_id: String,
    pub sop_greeting: Option<String>,
}

impl AgentSettings {
    pub fn from_env() -> Self {
        Self {
            xai: XaiSettings::from_env(),
            vad: VadSettings::from_env(),
            esl: EslSettings::from_env(),
            listen_port: num("VOICE_AI_REALTIME_PORT", 3100),
            result_dir: var_or(
                "VOICE_AI_PILOT_RESULT_DIR",
                "docker/sip-stack/runtime/pilot/results",
            )
            .into(),
            media_dir: var_or(
                "VOICE_AI_PILOT_MEDIA_DIR",
                "docker/sip-stack/runtime/pilot/media",
            )
            .into(),
            sample_rate: num("VOICE_AI_REALTIME_SAMPLE_RATE", 8000),
            local_sample_rate: num("VOICE_AI_LOCAL_SAMPLE_RATE", 24000),
            input_device: var("VOICE_AI_INPUT_DEVICE"),
            output_device: var("VOICE_AI_OUTPUT_DEVICE"),
            clients_dir: var_or("VOICE_AI_CLIENTS_DIR", "docker/sip-stack/clients").into(),
            default_client_id: var_or("VOICE_AI_DEFAULT_CLIENT_ID", "default"),
            sop_greeting: var("VOICE_AI_SOP_GREETING"),
        }
    }
}

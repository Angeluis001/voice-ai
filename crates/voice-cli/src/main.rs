//! `voice` — the desktop voice CLI: mic -> speech-to-text -> Grok -> text-to-speech -> playback.
mod audio;
mod ingest;
mod pipeline;

use anyhow::bail;
use clap::{Parser, Subcommand};
use serde_json::json;
use std::path::PathBuf;
use voice_core::{
    config::{self, XaiSettings},
    xai::XaiRest,
};

#[derive(Parser)]
#[command(name = "voice", about = "Voice AI MVP CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Record, transcribe, reason, synthesize, and play one interaction.
    Run,
    /// Validate required local configuration.
    CheckEnv,
    /// Record a WAV file without calling xAI.
    RecordOnly {
        /// Override record duration.
        #[arg(long)]
        seconds: Option<u32>,
    },
    /// Test Grok and optional TTS without microphone input.
    TextSmoke {
        /// Transcript text to send into the reasoning pipeline.
        #[arg(long)]
        text: String,
        /// Also synthesize the response to a WAV artifact.
        #[arg(long)]
        speak: bool,
    },
    /// Turn a folder of call recordings into console sessions with summaries.
    Ingest {
        /// Folder holding the recordings (wav, mp3, m4a, ogg, flac).
        #[arg(long)]
        source: PathBuf,
        /// Where sessions, media and the manifest are written.
        #[arg(long, default_value = "artifacts/sbr-ingest")]
        out: PathBuf,
        /// Client pack used for the summaries.
        #[arg(long, default_value = "sbr")]
        client: String,
    },
}

struct Settings {
    xai: XaiSettings,
    record_seconds: u32,
    sample_rate: u32,
    artifacts_dir: PathBuf,
    logs_dir: PathBuf,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn load_settings() -> anyhow::Result<Settings> {
    let xai = XaiSettings::from_env();
    if xai.api_key.is_empty() {
        bail!("XAI_API_KEY is missing. Add it to your local environment or .env file.");
    }
    let settings = Settings {
        xai,
        record_seconds: env("VOICE_MVP_RECORD_SECONDS")
            .and_then(|v| v.parse().ok())
            .unwrap_or(5),
        sample_rate: env("VOICE_MVP_SAMPLE_RATE")
            .and_then(|v| v.parse().ok())
            .unwrap_or(16000),
        artifacts_dir: env("VOICE_MVP_ARTIFACTS_DIR")
            .or_else(|| env("VOICE_IN_ARTIFACTS_DIR"))
            .unwrap_or_else(|| "artifacts".into())
            .into(),
        logs_dir: env("VOICE_MVP_LOGS_DIR")
            .unwrap_or_else(|| "logs".into())
            .into(),
    };
    // A read-only mount is fine: the directories are only needed when writing.
    let _ = std::fs::create_dir_all(&settings.artifacts_dir);
    let _ = std::fs::create_dir_all(&settings.logs_dir);
    Ok(settings)
}

fn print_json(value: &impl serde::Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    config::load_dotenv();
    let cli = Cli::parse();
    let settings = load_settings()?;
    match cli.command {
        Command::CheckEnv => print_json(&json!({
            "xai_api_key": "***redacted***",
            "xai_base_url": settings.xai.base_url,
            "language": settings.xai.language,
            "stt_keyterm": settings.xai.stt_keyterm,
            "grok_model": settings.xai.grok_model,
            "tts_voice": settings.xai.tts_voice,
            "record_seconds": settings.record_seconds,
            "sample_rate": settings.sample_rate,
            "artifacts_dir": settings.artifacts_dir,
            "logs_dir": settings.logs_dir,
        })),
        Command::RecordOnly { seconds } => {
            let seconds = seconds
                .filter(|s| *s > 0)
                .unwrap_or(settings.record_seconds);
            let output = settings.artifacts_dir.join("manual-recording.wav");
            let (path, rate) = (output.clone(), settings.sample_rate);
            tokio::task::spawn_blocking(move || audio::record_wav(&path, rate, seconds)).await??;
            println!("Saved recording to {}", output.display());
            Ok(())
        }
        Command::TextSmoke { text, speak } => {
            let xai = XaiRest::new(settings.xai.clone());
            let result = pipeline::reason_over_transcript(&xai, &text).await?;
            let mut payload = json!({"reasoning_result": result});
            if speak {
                let request_id = uuid::Uuid::new_v4().to_string();
                let output = settings.artifacts_dir.join("text-smoke-output.wav");
                let synthesis =
                    pipeline::synthesize_to_wav(&xai, &request_id, &result.response, &output)
                        .await?;
                payload["synthesis_event"] = serde_json::to_value(synthesis)?;
            }
            print_json(&payload)
        }
        Command::Ingest {
            source,
            out,
            client,
        } => {
            let xai = XaiRest::new(settings.xai.clone());
            let clients_dir = env("VOICE_AI_CLIENTS_DIR")
                .unwrap_or_else(|| "docker/sip-stack/clients".into())
                .into();
            let options = ingest::IngestOptions {
                source_dir: source,
                out_dir: out,
                clients_dir,
                client_id: client,
            };
            if ingest::run(&xai, &options).await? == 0 {
                bail!("no recording was summarised");
            }
            Ok(())
        }
        Command::Run => {
            let xai = XaiRest::new(settings.xai.clone());
            let (capture, output) =
                pipeline::artifact_paths(&settings.artifacts_dir, &pipeline::stamp());
            let (path, rate, seconds) = (
                capture.clone(),
                settings.sample_rate,
                settings.record_seconds,
            );
            eprintln!("Recording {seconds} s...");
            tokio::task::spawn_blocking(move || audio::record_wav(&path, rate, seconds)).await??;
            let interaction = pipeline::process_recording(&xai, &capture, &output).await?;
            let play_path = output.clone();
            tokio::task::spawn_blocking(move || audio::play_wav(&play_path)).await??;
            pipeline::append_jsonl(&settings.logs_dir.join("interactions.jsonl"), &interaction)?;
            print_json(&interaction)
        }
    }
}

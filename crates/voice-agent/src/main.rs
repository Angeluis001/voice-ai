use clap::{Parser, Subcommand};
use voice_agent::{
    serve::{self, ServeState},
    talk::{self, TalkOptions},
};
use voice_core::{call::CallDeps, config, session::SessionStore, util::field};

#[derive(Parser)]
#[command(name = "voice-agent", about = "Realtime Grok voice agent")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Answer softphone calls: listen for FreeSWITCH audio forks on the realtime port.
    Serve,
    /// Talk to the agent through this PC's microphone and speakers.
    Talk {
        /// Client pack to use (folder name under the clients directory).
        #[arg(long)]
        client: Option<String>,
        /// Training mode: the AI plays the customer and you play the agent.
        #[arg(long)]
        roleplay: bool,
        /// Roleplay scenario key (for example: fees, unsupported, transfer, angry).
        #[arg(long, default_value = "")]
        scenario: String,
        /// Mute the microphone while the AI speaks. Use this without a headset.
        #[arg(long)]
        half_duplex: bool,
        /// Play a WAV file as the caller instead of using the microphone. Repeat for
        /// several utterances; each is spoken after the AI finishes.
        #[arg(long = "caller-wav")]
        caller_wav: Vec<std::path::PathBuf>,
        /// Hang up automatically after this many seconds.
        #[arg(long)]
        seconds: Option<u64>,
    },
    /// List the microphones and speakers on this PC.
    Devices,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    config::load_dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let settings = config::AgentSettings::from_env();
    match Cli::parse().command {
        Command::Serve => serve::run(ServeState::new(CallDeps::new(settings))).await,
        Command::Devices => talk::list_devices(),
        Command::Talk {
            client,
            roleplay,
            scenario,
            half_duplex,
            caller_wav,
            seconds,
        } => {
            let options = TalkOptions {
                client_id: client,
                roleplay,
                scenario,
                half_duplex,
                max_seconds: seconds,
                caller_wavs: caller_wav,
            };
            let outcome = talk::run(CallDeps::new(settings), options).await?;
            println!("\nCall ended ({}).", outcome.reason);
            println!("Session:   {}", outcome.session_path.display());
            if let Some(doc) = SessionStore::load(&outcome.session_path).await {
                println!("Recording: {}", field(&doc, "recording_path"));
                println!("Status:    {}", field(&doc, "status"));
                if let Some(analysis) = doc.get("post_analysis").and_then(|a| a.as_object()) {
                    println!("Summary:   {}", field(analysis, "summary"));
                }
            }
            Ok(())
        }
    }
}

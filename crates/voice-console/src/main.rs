use clap::Parser;
use std::{path::PathBuf, sync::Arc};
use voice_brand::{BrandSettings, BrandStore};
use voice_console::{
    app::{router, AppState, LinphoneAccount},
    auth::AuthSettings,
};
use voice_core::{
    config::{self, XaiSettings},
    originate::OriginateEnv,
    xai::XaiRest,
};
use voice_sessions::{service::SessionService, settings::ConsoleSettings};

#[derive(Parser)]
#[command(name = "voice-console", about = "Voice IN console web server")]
struct Cli {
    /// Address to listen on. Use 0.0.0.0 to allow other devices on the network.
    #[arg(long, default_value = "0.0.0.0", env = "VOICE_IN_HOST")]
    host: String,
    #[arg(long, default_value_t = 8000, env = "VOICE_IN_PORT")]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    config::load_dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();

    let settings = ConsoleSettings::from_env();
    let auth = AuthSettings::from_env(&settings.base_path).map_err(anyhow::Error::msg)?;
    if auth.enabled {
        tracing::warn!(
            "Microsoft login enabled for tenant {} (client {})",
            auth.tenant_id,
            auth.client_id
        );
    } else {
        tracing::warn!("Microsoft login disabled — Voice IN console is open without sign-in.");
    }
    let xai = XaiRest::new(XaiSettings::from_env());
    let brand_settings = BrandSettings {
        brand_dir: settings.brand_dir.clone(),
        clients_dir: settings.clients_dir.clone(),
        default_client_id: settings.default_client_id.clone(),
    };
    let sessions = SessionService::new(settings, xai.clone());
    sessions.ensure_dirs();
    let state = AppState {
        sessions,
        brand: BrandStore::new(brand_settings, Arc::new(xai)),
        originate: Arc::new(OriginateEnv::from_env()),
        linphone: LinphoneAccount::from_env().map(Arc::new),
        qr_dir: PathBuf::from("docs/sip/linphone-qr"),
        auth: Arc::new(auth),
        http: reqwest::Client::new(),
    };

    let listener = tokio::net::TcpListener::bind((cli.host.as_str(), cli.port)).await?;
    tracing::info!(
        "Voice IN console on http://127.0.0.1:{} (listening on {}:{})",
        cli.port,
        cli.host,
        cli.port
    );
    axum::serve(listener, router(state)).await?;
    Ok(())
}

//! Softphone mode: FreeSWITCH `uuid_audio_fork` connects to `ws://<host>:3100/media`
//! and streams the caller's audio as raw L16 mono frames. AI audio goes back the same
//! way, one binary frame per chunk, with no re-chunking or pacing.
//!
//! Playout is cleared and calls are transferred over the FreeSWITCH event socket,
//! never by sending control JSON down the fork socket (that closes the fork).
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::Response,
    routing::get,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::sync::mpsc;
use voice_core::{
    call::{run_call, CallDeps, CallParams, LegCommand, LegEvent},
    esl::{stop_play_command, transfer_command, EslClient},
};

#[derive(Clone)]
pub struct ServeState {
    pub deps: CallDeps,
    pub esl: EslClient,
}

impl ServeState {
    pub fn new(deps: CallDeps) -> Self {
        let esl = EslClient::new(deps.settings.esl.clone());
        Self { deps, esl }
    }
}

pub fn router(state: ServeState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/media", get(media))
        .with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({"ok": true, "service": "voice-ai-realtime-bridge"}))
}

async fn media(
    ws: WebSocketUpgrade,
    Query(query): Query<HashMap<String, String>>,
    State(state): State<ServeState>,
) -> Response {
    // FreeSWITCH mod_audio_fork negotiates this subprotocol.
    ws.protocols(["audio.drachtio.org"])
        .on_upgrade(move |socket| handle_fork(socket, query, state))
}

async fn handle_fork(socket: WebSocket, query: HashMap<String, String>, state: ServeState) {
    let params = CallParams::from_query(&query, &state.deps.settings);
    let (call_id, uuid) = (params.call_id.clone(), params.channel_uuid.clone());
    let (event_tx, event_rx) = mpsc::channel::<LegEvent>(2048);
    let (command_tx, mut command_rx) = mpsc::channel::<LegCommand>(2048);
    let (mut sink, mut stream) = socket.split();

    let reader = {
        let call_id = call_id.clone();
        tokio::spawn(async move {
            loop {
                let event = match stream.next().await {
                    Some(Ok(Message::Binary(frame))) => LegEvent::Audio(frame.to_vec()),
                    Some(Ok(Message::Text(text))) => {
                        let kind = serde_json::from_str::<Value>(text.as_str())
                            .ok()
                            .and_then(|m| {
                                m["type"]
                                    .as_str()
                                    .or(m["event"].as_str())
                                    .map(str::to_string)
                            })
                            .unwrap_or_else(|| "json".into());
                        tracing::debug!(phase = "fork_json", call_id = %call_id, kind);
                        continue;
                    }
                    Some(Ok(Message::Close(_))) | None => LegEvent::Closed,
                    Some(Ok(_)) => continue,
                    Some(Err(error)) => LegEvent::Error(error.to_string()),
                };
                let done = !matches!(event, LegEvent::Audio(_));
                if event_tx.send(event).await.is_err() || done {
                    break;
                }
            }
        })
    };

    let writer = {
        let (esl, call_id, uuid) = (state.esl.clone(), call_id.clone(), uuid.clone());
        tokio::spawn(async move {
            while let Some(command) = command_rx.recv().await {
                match command {
                    LegCommand::Audio(pcm) => {
                        if sink.send(Message::Binary(pcm.into())).await.is_err() {
                            break;
                        }
                    }
                    LegCommand::ClearPlayout => {
                        if uuid.is_empty() {
                            continue;
                        }
                        let (esl, call_id, uuid) = (esl.clone(), call_id.clone(), uuid.clone());
                        tokio::spawn(async move {
                            let result = esl.api(&stop_play_command(&uuid)).await;
                            tracing::info!(phase = "barge_in_fs_clear", call_id = %call_id, uuid = %uuid, ?result);
                        });
                    }
                    LegCommand::Transfer { extension } => {
                        if uuid.is_empty() {
                            continue;
                        }
                        let _ = esl.api(&stop_play_command(&uuid)).await;
                        let result = esl.api(&transfer_command(&uuid, &extension)).await;
                        tracing::info!(phase = "transfer_executed", call_id = %call_id, uuid = %uuid, ?result);
                    }
                }
            }
            let _ = sink.close().await;
        })
    };

    let outcome = run_call(state.deps.clone(), params, event_rx, command_tx).await;
    tracing::info!(phase = "call_finished", call_id = %call_id, reason = %outcome.reason);
    reader.abort();
    let _ = writer.await;
}

/// Listen for audio forks until the process is stopped.
pub async fn run(state: ServeState) -> anyhow::Result<()> {
    let settings = state.deps.settings.clone();
    tokio::fs::create_dir_all(&settings.result_dir).await?;
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", settings.listen_port)).await?;
    tracing::info!(
        phase = "listening",
        port = settings.listen_port,
        sample_rate = settings.sample_rate,
        realtime_url = %settings.xai.realtime_url
    );
    axum::serve(listener, router(state)).await?;
    Ok(())
}

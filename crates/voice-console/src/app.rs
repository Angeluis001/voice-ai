//! The Voice IN console web server: pages, telephony APIs, live session feed (SSE),
//! brand voice APIs, device upload, click-to-call and Linphone provisioning.
//!
//! Errors are returned as `{"detail": ...}` JSON, which the browser code expects.
use crate::{
    auth::{self, AuthSettings},
    ui::{self, UiContext},
};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path as UrlPath, Query, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse, Response,
    },
    routing::get,
    Json, Router,
};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{convert::Infallible, path::PathBuf, sync::Arc, time::Duration};
use tower::ServiceExt;
use tower_http::services::ServeFile;
use voice_brand::{BrandError, BrandMessage, BrandStore, CloneRequest, SynthesizeRequest};
use voice_core::originate::{originate, OriginateEnv, OriginateRequest};
use voice_sessions::{
    models::LiveCallSession,
    service::{sessions_fingerprint, ServiceError, SessionService},
};

/// Softphone account handed out by the Linphone provisioning endpoint.
#[derive(Debug, Clone)]
pub struct LinphoneAccount {
    pub extension: String,
    pub password: String,
    pub domain: String,
    pub proxy_host: String,
    pub proxy_port: String,
    pub transport: String,
    pub display_name: String,
    pub did: String,
}

impl LinphoneAccount {
    /// Extension 1001 from the environment. The password is never built into the binary:
    /// without `VOICE_LINPHONE_1001_PASSWORD` provisioning is switched off.
    pub fn from_env() -> Option<Self> {
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        Some(Self {
            extension: "1001".into(),
            password: var("VOICE_LINPHONE_1001_PASSWORD")?,
            domain: var("VOICE_LINPHONE_DOMAIN").unwrap_or_else(|| "voiceai.local".into()),
            proxy_host: var("VOICE_LINPHONE_PROXY_HOST").unwrap_or_else(|| "100.76.98.45".into()),
            proxy_port: var("VOICE_LINPHONE_PROXY_PORT").unwrap_or_else(|| "5062".into()),
            transport: "udp".into(),
            display_name: "Voice AI iPhone".into(),
            did: "0734952902".into(),
        })
    }
}

#[derive(Clone)]
pub struct AppState {
    pub sessions: SessionService,
    pub brand: BrandStore,
    pub originate: Arc<OriginateEnv>,
    pub linphone: Option<Arc<LinphoneAccount>>,
    pub qr_dir: PathBuf,
    pub auth: Arc<AuthSettings>,
    /// HTTP client for the Microsoft token exchange.
    pub http: reqwest::Client,
}

impl AppState {
    fn ctx(&self) -> UiContext {
        self.auth.ui_context()
    }

    /// Session listing touches the file system; keep it off the async workers.
    async fn list(&self, limit: usize) -> Vec<LiveCallSession> {
        let service = self.sessions.clone();
        tokio::task::spawn_blocking(move || service.list_live_call_sessions(limit))
            .await
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

pub struct ApiError(StatusCode, Value);

impl ApiError {
    fn new(status: StatusCode, detail: impl Into<Value>) -> Self {
        Self(status, detail.into())
    }
    fn not_found(detail: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, detail)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"detail": self.1}))).into_response()
    }
}

impl From<BrandError> for ApiError {
    fn from(error: BrandError) -> Self {
        let status = StatusCode::from_u16(error.status_code).unwrap_or(StatusCode::BAD_REQUEST);
        Self::new(status, error.message)
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// FastAPI-style validation error for a missing required body field.
fn missing_field(field: &str) -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!([{"type": "missing", "loc": ["body", field], "msg": "Field required"}]),
    )
}

#[derive(Deserialize)]
struct LimitQuery {
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    10
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

async fn home(State(s): State<AppState>) -> Html<String> {
    Html(ui::render_home(&s.ctx(), &s.list(12).await))
}
async fn live_console(State(s): State<AppState>) -> Html<String> {
    Html(ui::render_console(&s.ctx(), &s.list(10).await))
}
async fn assist_page(State(s): State<AppState>) -> Html<String> {
    Html(ui::render_assist(&s.ctx(), &s.list(15).await))
}
async fn record_page(State(s): State<AppState>) -> Html<String> {
    Html(ui::render_record(&s.ctx(), &s.list(50).await))
}
async fn capture_page(State(s): State<AppState>) -> Html<String> {
    Html(ui::render_capture(&s.ctx(), &s.list(30).await))
}
async fn create_page(State(s): State<AppState>) -> Html<String> {
    Html(ui::render_create(&s.ctx(), &s.list(30).await))
}
async fn brand_page(State(s): State<AppState>) -> Html<String> {
    Html(ui::render_brand(&s.ctx(), &voice_brand::TTS_LANGUAGES))
}

async fn train_page(State(s): State<AppState>) -> Html<String> {
    let mut pack = s.sessions.load_client_pack(None);
    let has_tree = |p: &Value| {
        p["roleplay"]["tree"]
            .as_object()
            .map(|t| !t.is_empty())
            .unwrap_or(false)
    };
    if !has_tree(&pack) {
        // The training tree lives in the SBR pack when the default pack has none.
        let sbr = s.sessions.load_client_pack(Some("sbr"));
        if sbr.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            pack = sbr;
        }
    }
    Html(ui::render_train(&s.ctx(), &s.list(30).await, &pack))
}

async fn summary_page(
    State(s): State<AppState>,
    UrlPath(session_id): UrlPath<String>,
) -> ApiResult<Html<String>> {
    let session = match s.sessions.ensure_post_analysis(&session_id, false).await {
        Ok(session) => session,
        Err(ServiceError::NotFound(message)) => return Err(ApiError::not_found(&message)),
        Err(error) => {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Summary failed: {error}"),
            ))
        }
    };
    let analysis = session.post_analysis.clone().unwrap_or_default();
    Ok(Html(ui::render_summary(&s.ctx(), &session, &analysis)))
}

// ---------------------------------------------------------------------------
// Telephony APIs
// ---------------------------------------------------------------------------

async fn telephony_recent(State(s): State<AppState>, Query(q): Query<LimitQuery>) -> Json<Value> {
    let service = s.sessions.clone();
    let results =
        tokio::task::spawn_blocking(move || service.list_recent_telephony_results(q.limit))
            .await
            .unwrap_or_default();
    Json(json!(results))
}

async fn telephony_sessions(State(s): State<AppState>, Query(q): Query<LimitQuery>) -> Json<Value> {
    Json(json!(s.list(q.limit).await))
}

async fn find_session(s: &AppState, session_id: String) -> Option<LiveCallSession> {
    let service = s.sessions.clone();
    tokio::task::spawn_blocking(move || service.get_session_by_id(&session_id))
        .await
        .ok()
        .flatten()
}

async fn telephony_session_detail(
    State(s): State<AppState>,
    UrlPath(session_id): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    find_session(&s, session_id)
        .await
        .map(|session| Json(json!(session)))
        .ok_or_else(|| ApiError::not_found("Session not found"))
}

#[derive(Deserialize)]
struct ForceQuery {
    #[serde(default)]
    force: bool,
}

async fn telephony_session_summary(
    State(s): State<AppState>,
    UrlPath(session_id): UrlPath<String>,
    Query(q): Query<ForceQuery>,
) -> ApiResult<Json<Value>> {
    let session = match s.sessions.ensure_post_analysis(&session_id, q.force).await {
        Ok(session) => session,
        Err(ServiceError::NotFound(message)) => return Err(ApiError::not_found(&message)),
        Err(error) => {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            ))
        }
    };
    let mut analysis = session.post_analysis.clone().unwrap_or_default();
    let handoff = [
        session.handoff_brief.trim(),
        analysis.internal_handoff_brief.trim(),
    ]
    .into_iter()
    .find(|text| !text.is_empty())
    .unwrap_or_default()
    .to_string();
    if !handoff.is_empty() && analysis.internal_handoff_brief.trim().is_empty() {
        analysis.internal_handoff_brief = handoff.clone();
    }
    Ok(Json(json!({
        "session_id": session.session_id,
        "call_id": session.call_id,
        "status": session.status,
        "target_number": session.target_number,
        "turn_count": session.turn_count,
        "recording_path": session.recording_path,
        "has_recording": session.has_recording,
        "handoff_brief": handoff,
        "content": session.content,
        "analysis": analysis,
    })))
}

/// Serve a file with Range support, a fixed media type and a download name.
async fn serve_file(
    path: PathBuf,
    request: Request,
    media_type: &str,
    disposition: &str,
    filename: &str,
) -> ApiResult<Response> {
    // ServeFile cannot fail: a missing file becomes a 404 response.
    let Ok(response) = ServeFile::new(&path).oneshot(request).await;
    let mut response = response.map(Body::new);
    let safe_name: String = filename
        .chars()
        .filter(|c| c.is_ascii() && *c != '"' && !c.is_control())
        .collect();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(media_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=60"),
    );
    if let Ok(value) = HeaderValue::from_str(&format!("{disposition}; filename=\"{safe_name}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    Ok(response)
}

async fn telephony_session_recording(
    State(s): State<AppState>,
    UrlPath(session_id): UrlPath<String>,
    request: Request,
) -> ApiResult<Response> {
    let service = s.sessions.clone();
    let path = tokio::task::spawn_blocking(move || {
        service
            .get_session_by_id(&session_id)
            .and_then(|session| service.resolve_recording_file(&session))
    })
    .await
    .ok()
    .flatten()
    .filter(|p| p.exists())
    .ok_or_else(|| ApiError::not_found("Recording not found"))?;
    let media_type = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("ogg") => "audio/ogg",
        Some("flac") => "audio/flac",
        _ => "application/octet-stream",
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("recording")
        .to_string();
    serve_file(path, request, media_type, "attachment", &name).await
}

/// Live feed: a full snapshot of recent sessions whenever anything visible changes.
async fn telephony_events(
    State(s): State<AppState>,
    Query(q): Query<LimitQuery>,
) -> impl IntoResponse {
    let stream = session_event_stream(s.sessions.clone(), q.limit);
    let sse = Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10)));
    (
        [
            (header::CACHE_CONTROL, "no-cache"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        sse,
    )
}

fn session_event_stream(
    service: SessionService,
    limit: usize,
) -> impl Stream<Item = Result<Event, Infallible>> {
    struct Feed {
        service: SessionService,
        limit: usize,
        last_fingerprint: Option<String>,
        last_signature: String,
        ticks: u64,
    }
    let feed = Feed {
        service,
        limit,
        last_fingerprint: None,
        last_signature: String::new(),
        ticks: 0,
    };
    futures_util::stream::unfold(feed, |mut feed| async move {
        loop {
            if feed.ticks > 0 {
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            feed.ticks += 1;
            let (service, limit, previous) = (
                feed.service.clone(),
                feed.limit,
                feed.last_signature.clone(),
            );
            // Rescan when the results directory changed, and every 4th tick regardless.
            let force = feed.ticks % 4 == 0 || feed.last_fingerprint.is_none();
            let scan = tokio::task::spawn_blocking(move || {
                let signature = service.results_dir_signature();
                if signature == previous && !force {
                    return (signature, None);
                }
                let sessions = service.list_live_call_sessions(limit);
                (signature, Some(sessions))
            })
            .await;
            let Ok((signature, sessions)) = scan else {
                let event = Event::default()
                    .event("error")
                    .data(json!({"detail": "session scan failed"}).to_string());
                return Some((Ok(event), feed));
            };
            feed.last_signature = signature;
            let Some(sessions) = sessions else { continue };
            let fingerprint = sessions_fingerprint(&sessions);
            if feed.last_fingerprint.as_deref() == Some(fingerprint.as_str()) {
                continue;
            }
            feed.last_fingerprint = Some(fingerprint);
            let event = Event::default()
                .event("sessions")
                .data(json!(sessions).to_string());
            return Some((Ok(event), feed));
        }
    })
}

#[derive(Deserialize)]
struct OriginateBody {
    to: Option<String>,
    #[serde(default)]
    stable_entity_id: String,
    #[serde(default)]
    related_matter_seid: String,
    #[serde(default)]
    operator_id: String,
    #[serde(default)]
    command_id: String,
    #[serde(default)]
    dry_run: bool,
}

async fn telephony_originate(
    State(s): State<AppState>,
    Json(body): Json<OriginateBody>,
) -> ApiResult<Json<Value>> {
    let request = OriginateRequest {
        to: body.to.ok_or_else(|| missing_field("to"))?,
        stable_entity_id: body.stable_entity_id,
        related_matter_seid: body.related_matter_seid,
        operator_id: body.operator_id,
        command_id: body.command_id,
        dry_run: body.dry_run,
    };
    let result = originate(&request, &s.originate).await;
    if result["ok"].as_bool() != Some(true) {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, result));
    }
    Ok(Json(result))
}

// ---------------------------------------------------------------------------
// Device upload
// ---------------------------------------------------------------------------

async fn device_upload(
    State(s): State<AppState>,
    mut multipart: Multipart,
) -> ApiResult<Json<Value>> {
    let bad = |detail: &str| ApiError::new(StatusCode::BAD_REQUEST, detail);
    let mut upload = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| bad(&e.to_string()))?
    {
        if field.name() == Some("file") {
            let filename = field.file_name().unwrap_or_default().to_string();
            let bytes = field.bytes().await.map_err(|e| bad(&e.to_string()))?;
            upload = Some((filename, bytes.to_vec()));
        }
    }
    let (filename, bytes) = upload.ok_or_else(|| missing_field("file"))?;
    if filename.is_empty() {
        return Err(bad("Missing upload filename"));
    }
    if !filename.to_lowercase().ends_with(".wav") {
        return Err(bad("Only WAV uploads are supported in this MVP"));
    }
    let result = s
        .sessions
        .process_device_upload(&filename, bytes)
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!(result)))
}

// ---------------------------------------------------------------------------
// Brand voice APIs
// ---------------------------------------------------------------------------

fn message_payload(s: &AppState, message: &BrandMessage) -> Value {
    let audio = s.ctx().path(&format!(
        "/api/brand/{}/messages/{}/audio",
        message.client_id, message.id
    ));
    json!({"message": message, "audio_url": audio, "download_url": format!("{audio}?download=1")})
}

async fn brand_clients(State(s): State<AppState>) -> Json<Value> {
    let kinds: serde_json::Map<String, Value> = voice_brand::MESSAGE_KINDS
        .iter()
        .map(|(k, label)| (k.to_string(), json!(label)))
        .collect();
    let languages: Vec<Value> = voice_brand::TTS_LANGUAGES
        .iter()
        .map(|(code, label)| json!({"code": code, "label": label}))
        .collect();
    Json(json!({
        "clients": s.brand.list_clients(),
        "default_client_id": s.brand.default_client_id(),
        "kinds": kinds,
        "languages": languages,
        "speed_min": voice_brand::SPEED_MIN,
        "speed_max": voice_brand::SPEED_MAX,
    }))
}

async fn brand_voices(
    State(s): State<AppState>,
    UrlPath(client_id): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.brand.list_voices(&client_id).await?)))
}

#[derive(Deserialize)]
struct TemplateQuery {
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default)]
    starts_on: String,
    #[serde(default)]
    ends_on: String,
}
fn default_kind() -> String {
    "closed".into()
}

async fn brand_template(
    State(s): State<AppState>,
    UrlPath(client_id): UrlPath<String>,
    Query(q): Query<TemplateQuery>,
) -> ApiResult<Json<Value>> {
    let client = s.brand.get_client(&client_id)?;
    Ok(Json(json!({
        "script": s.brand.script_template(&client, &q.kind, &q.starts_on, &q.ends_on),
        "title": voice_brand::template_title(&q.kind, &q.starts_on, &q.ends_on),
        "company": client.company,
    })))
}

async fn brand_clone(
    State(s): State<AppState>,
    UrlPath(client_id): UrlPath<String>,
    mut multipart: Multipart,
) -> ApiResult<Json<Value>> {
    let bad = |detail: String| ApiError::new(StatusCode::BAD_REQUEST, detail);
    let mut request = CloneRequest::default();
    let mut has_file = false;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| bad(e.to_string()))?
    {
        match field.name().unwrap_or_default() {
            "file" => {
                has_file = true;
                request.filename = field.file_name().unwrap_or_default().to_string();
                request.content_type = field.content_type().unwrap_or("audio/wav").to_string();
                request.bytes = field
                    .bytes()
                    .await
                    .map_err(|e| bad(e.to_string()))?
                    .to_vec();
            }
            name @ ("name" | "gender" | "tone" | "language") => {
                let name = name.to_string();
                let text = field.text().await.map_err(|e| bad(e.to_string()))?;
                match name.as_str() {
                    "name" => request.name = text,
                    "gender" => request.gender = text,
                    "tone" => request.tone = text,
                    _ => request.language = text,
                }
            }
            _ => {}
        }
    }
    if !has_file {
        return Err(missing_field("file"));
    }
    if request.name.is_empty() {
        request.name = if request.filename.is_empty() {
            "Cloned voice".into()
        } else {
            request.filename.clone()
        };
    }
    if request.filename.is_empty() {
        request.filename = "reference.wav".into();
    }
    let voice = s.brand.clone_voice_from_sample(&client_id, request).await?;
    Ok(Json(json!({"voice": voice})))
}

fn text_field(body: &Value, key: &str) -> String {
    body[key].as_str().unwrap_or_default().to_string()
}

async fn brand_register_voice(
    State(s): State<AppState>,
    UrlPath(client_id): UrlPath<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let voice_id = body["voice_id"]
        .as_str()
        .ok_or_else(|| missing_field("voice_id"))?;
    let voice = s.brand.register_cloned_voice(
        &client_id,
        voice_id,
        &text_field(&body, "name"),
        &text_field(&body, "description"),
        &text_field(&body, "language"),
    )?;
    Ok(Json(json!({"voice": voice})))
}

async fn brand_list_messages(
    State(s): State<AppState>,
    UrlPath(client_id): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({"messages": s.brand.list_messages(&client_id)?}),
    ))
}

async fn brand_create_message(
    State(s): State<AppState>,
    UrlPath(client_id): UrlPath<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    for required in ["script", "voice_id"] {
        if !body[required].is_string() {
            return Err(missing_field(required));
        }
    }
    let kind = body["kind"].as_str().unwrap_or("custom").to_string();
    let request = SynthesizeRequest {
        title: text_field(&body, "title"),
        kind,
        script: text_field(&body, "script"),
        voice_id: text_field(&body, "voice_id"),
        voice_name: text_field(&body, "voice_name"),
        language: text_field(&body, "language"),
        speed: voice_brand::normalize_speed(body.get("speed").filter(|v| !v.is_null()))?,
        starts_on: text_field(&body, "starts_on"),
        ends_on: text_field(&body, "ends_on"),
    };
    let message = s.brand.synthesize_message(&client_id, request).await?;
    Ok(Json(message_payload(&s, &message)))
}

async fn brand_get_message(
    State(s): State<AppState>,
    UrlPath((client_id, message_id)): UrlPath<(String, String)>,
) -> ApiResult<Json<Value>> {
    let message = s.brand.get_message(&client_id, &message_id)?;
    Ok(Json(message_payload(&s, &message)))
}

#[derive(Deserialize)]
struct DownloadQuery {
    #[serde(default)]
    download: Option<String>,
}

async fn brand_message_audio(
    State(s): State<AppState>,
    UrlPath((client_id, message_id)): UrlPath<(String, String)>,
    Query(q): Query<DownloadQuery>,
    request: Request,
) -> ApiResult<Response> {
    let message = s.brand.get_message(&client_id, &message_id)?;
    let wav = s.brand.message_audio_path(&client_id, &message_id)?;
    if !wav.is_file() {
        return Err(ApiError::not_found("Audio file not found"));
    }
    let download = matches!(
        q.download.as_deref().map(str::to_lowercase).as_deref(),
        Some("1" | "true" | "yes" | "on")
    );
    let disposition = if download { "attachment" } else { "inline" };
    serve_file(
        wav,
        request,
        "audio/wav",
        disposition,
        &voice_brand::download_filename(&message),
    )
    .await
}

async fn brand_delete_message(
    State(s): State<AppState>,
    UrlPath((client_id, message_id)): UrlPath<(String, String)>,
) -> ApiResult<Json<Value>> {
    s.brand.delete_message(&client_id, &message_id)?;
    Ok(Json(json!({"deleted": true})))
}

// ---------------------------------------------------------------------------
// Linphone provisioning (softphone setup by QR code)
// ---------------------------------------------------------------------------

async fn linphone_qr_page(State(s): State<AppState>) -> Html<String> {
    let ctx = s.ctx();
    let account = s.linphone.as_deref();
    let host = account
        .map(|a| a.proxy_host.as_str())
        .unwrap_or("100.76.98.45");
    let port = account.map(|a| a.proxy_port.as_str()).unwrap_or("5062");
    let domain = account
        .map(|a| a.domain.as_str())
        .unwrap_or("voiceai.local");
    let image = |kind: &str| ui::escape(&ctx.path(&format!("/linphone/qr/image/{kind}")));
    Html(format!(
        r#"<!DOCTYPE html>
<html lang="en"><head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>Linphone QR · 1001</title>
<style>
  body {{ font-family: system-ui, sans-serif; background:#0b1220; color:#e8eef6; margin:0; padding:24px; }}
  h1 {{ font-size:1.25rem; margin:0 0 8px; }}
  p {{ color:#9aa8bc; line-height:1.45; max-width:36rem; }}
  .card {{ background:#121a24; border:1px solid #243044; border-radius:14px; padding:18px; margin:16px 0; max-width:420px; }}
  img {{ width:100%; max-width:320px; background:#fff; border-radius:8px; padding:10px; display:block; }}
  code {{ color:#c6a75e; }}
  .ok {{ color:#6ee7b7; font-weight:600; }}
</style>
</head><body>
  <h1>Linphone setup · extension 1001</h1>
  <p class="ok">DID 0734952902 · does not change 2900 / 2901 / transfer-to-1000</p>
  <p>On iPhone: open <strong>Tailscale</strong> (Connected), then open <strong>Linphone → Assistant / Scan QR code</strong>.
  Try QR #1 first; if it only fills the address, enter the password manually.</p>

  <div class="card">
    <h2 style="font-size:1rem;margin:0 0 10px">1) Full provision URL (preferred)</h2>
    <img src="{provision}" alt="Linphone provision QR"/>
    <p style="margin-top:10px;font-size:0.85rem">Encodes: <code>http://{host}:8000/api/linphone/provision/1001</code></p>
  </div>

  <div class="card">
    <h2 style="font-size:1rem;margin:0 0 10px">2) linphone-config:// scheme</h2>
    <img src="{scheme}" alt="Linphone config scheme QR"/>
  </div>

  <div class="card">
    <h2 style="font-size:1rem;margin:0 0 10px">3) SIP URI only (password manual)</h2>
    <img src="{sip}" alt="SIP URI QR"/>
    <p style="margin-top:10px;font-size:0.9rem">
      User <code>1001</code> · Domain <code>{domain}</code><br/>
      Password: ask your administrator<br/>
      Proxy: <code>{host}:{port}</code> UDP
    </p>
  </div>
</body></html>"#,
        provision = image("provision"),
        scheme = image("config-scheme"),
        sip = image("sip"),
        host = ui::escape(host),
        port = ui::escape(port),
        domain = ui::escape(domain),
    ))
}

async fn linphone_qr_image(
    State(s): State<AppState>,
    UrlPath(kind): UrlPath<String>,
    request: Request,
) -> ApiResult<Response> {
    let name = match kind.as_str() {
        "provision" => "linphone-provision-url.png",
        "config-scheme" => "linphone-config-scheme.png",
        "sip" => "sip-uri-basic.png",
        "sip-proxy" => "sip-uri-with-proxy.png",
        _ => return Err(ApiError::not_found("Unknown QR kind")),
    };
    let path = s.qr_dir.join(name);
    if !path.exists() {
        return Err(ApiError::not_found(
            "QR image missing — regenerate docs/sip/linphone-qr",
        ));
    }
    let Ok(response) = ServeFile::new(&path).oneshot(request).await;
    Ok(response.map(Body::new))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

async fn linphone_provision(
    State(s): State<AppState>,
    UrlPath(extension): UrlPath<String>,
) -> ApiResult<Response> {
    let account = s
        .linphone
        .as_deref()
        .filter(|a| a.extension == extension)
        .ok_or_else(|| ApiError::not_found("Unknown extension"))?;
    let (u, d, host, port) = (
        &account.extension,
        &account.domain,
        &account.proxy_host,
        &account.proxy_port,
    );
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<config xmlns="http://www.linphone.org/xsds/lpconfig.xsd"
        xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
        xsi:schemaLocation="http://www.linphone.org/xsds/lpconfig.xsd lpconfig.xsd">
  <section name="misc">
    <entry name="config-uri">http://{host}:8000/api/linphone/provision/{u}</entry>
    <entry name="transient_provisioning">1</entry>
  </section>
  <section name="proxy_0">
    <entry name="reg_proxy">&lt;sip:{host}:{port};transport={transport}&gt;</entry>
    <entry name="reg_identity">&lt;sip:{u}@{d}&gt;</entry>
    <entry name="reg_expires">3600</entry>
    <entry name="reg_sendregister">1</entry>
    <entry name="publish">0</entry>
    <entry name="dial_escape_plus">0</entry>
  </section>
  <section name="auth_info_0">
    <entry name="username">{u}</entry>
    <entry name="userid">{u}</entry>
    <entry name="passwd">{password}</entry>
    <entry name="realm">{d}</entry>
    <entry name="domain">{d}</entry>
  </section>
  <section name="sip">
    <entry name="default_proxy">0</entry>
    <entry name="display_name">{display}</entry>
  </section>
</config>
"#,
        transport = account.transport,
        password = xml_escape(&account.password),
        display = xml_escape(&account.display_name),
    );
    Ok((
        [
            (header::CONTENT_TYPE, "application/xml"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        xml,
    )
        .into_response())
}

async fn healthz(State(s): State<AppState>) -> Json<Value> {
    Json(json!({"ok": true, "auth": s.auth.enabled}))
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/console", get(live_console))
        .route("/assist", get(assist_page))
        .route("/record", get(record_page))
        .route("/capture", get(capture_page))
        .route("/train", get(train_page))
        .route("/create", get(create_page))
        .route("/brand", get(brand_page))
        .route("/summary/{session_id}", get(summary_page))
        .route("/linphone/qr", get(linphone_qr_page))
        .route("/linphone/qr/image/{kind}", get(linphone_qr_image))
        .route(
            "/api/linphone/provision/{extension}",
            get(linphone_provision),
        )
        .route("/api/telephony/recent", get(telephony_recent))
        .route(
            "/api/telephony/originate",
            axum::routing::post(telephony_originate),
        )
        .route("/api/telephony/sessions", get(telephony_sessions))
        .route(
            "/api/telephony/sessions/{session_id}",
            get(telephony_session_detail),
        )
        .route(
            "/api/telephony/sessions/{session_id}/summary",
            get(telephony_session_summary),
        )
        .route(
            "/api/telephony/sessions/{session_id}/recording",
            get(telephony_session_recording),
        )
        .route("/api/telephony/events", get(telephony_events))
        .route("/api/brand/clients", get(brand_clients))
        .route(
            "/api/brand/{client_id}/voices",
            get(brand_voices).post(brand_register_voice),
        )
        .route("/api/brand/{client_id}/template", get(brand_template))
        .route(
            "/api/brand/{client_id}/clone",
            axum::routing::post(brand_clone),
        )
        .route(
            "/api/brand/{client_id}/messages",
            get(brand_list_messages).post(brand_create_message),
        )
        .route(
            "/api/brand/{client_id}/messages/{message_id}",
            get(brand_get_message).delete(brand_delete_message),
        )
        .route(
            "/api/brand/{client_id}/messages/{message_id}/audio",
            get(brand_message_audio),
        )
        .route("/api/device/upload", axum::routing::post(device_upload))
        .route("/healthz", get(healthz))
        .route("/auth/login", get(auth::login))
        .route("/auth/callback", get(auth::callback))
        .route("/auth/logout", get(auth::logout))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::gate,
        ))
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(state)
}

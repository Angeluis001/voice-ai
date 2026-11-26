//! The console web server against a temporary data directory: pages render, APIs
//! answer in the shapes the browser code expects, and nothing needs xAI or FreeSWITCH.
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc};
use tower::ServiceExt;
use voice_brand::{BrandSettings, BrandStore};
use voice_console::{
    app::{router, AppState, LinphoneAccount},
    auth::AuthSettings,
};
use voice_core::{
    config::{EslSettings, XaiSettings},
    originate::OriginateEnv,
    xai::XaiRest,
};
use voice_sessions::{service::SessionService, settings::ConsoleSettings};

fn write(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_string_pretty(value).unwrap()).unwrap();
}

fn app(root: &Path, linphone_password: Option<&str>) -> Router {
    let settings = ConsoleSettings::for_dir(root);
    write(
        &settings.clients_dir.join("acme").join("client.json"),
        &json!({"id": "acme", "brand": {"company": "Acme Pty Ltd", "greeting": "Hello from Acme"}}),
    );
    write(
        &settings.telephony_results_dir.join("call-1-session.json"),
        &json!({
            "conversation_id": "call-1", "target_number": "9198", "status": "transferred", "mode": "realtime",
            "started_at": "2026-11-01T00:00:00.000Z", "updated_at": "2026-11-01T00:01:00.000Z",
            "latest_phase": "completed", "latest_intent": "request_transfer", "latest_action": "transfer",
            "slots": {"debt_type": "invoice", "amount": "$5,000", "action": "transfer"},
            "channel_uuid": "chan-1", "recording_path": "/var/lib/voice-ai/pilot/media/chan-1-call.wav",
            "turns": [
                {"turn": 0, "transcript": "", "response": "Good morning", "intent": "opening_greeting", "phase": "completed"},
                {"turn": 1, "transcript": "A person <please>", "response": "Transferring you", "intent": "request_transfer",
                 "action": "transfer", "phase": "completed", "request_id": "call-1-rt1", "call_id": "call-1"}
            ]
        }),
    );
    std::fs::create_dir_all(&settings.telephony_media_dir).unwrap();
    std::fs::write(
        settings.telephony_media_dir.join("chan-1-call.wav"),
        vec![7u8; 400],
    )
    .unwrap();

    let xai = XaiRest::new(XaiSettings {
        api_key: String::new(),
        base_url: "http://127.0.0.1:9".into(),
        realtime_url: String::new(),
        grok_model: "m".into(),
        tts_voice: "eve".into(),
        language: "en".into(),
        stt_keyterm: None,
    });
    let brand = BrandSettings {
        brand_dir: settings.brand_dir.clone(),
        clients_dir: settings.clients_dir.clone(),
        default_client_id: "acme".into(),
    };
    let sessions = SessionService::new(settings, xai.clone());
    sessions.ensure_dirs();
    router(AppState {
        sessions,
        brand: BrandStore::new(brand, Arc::new(xai)),
        originate: Arc::new(OriginateEnv {
            rules_path: None,
            operator_endpoint: None,
            outbound_cli: None,
            outbound_cli_name: None,
            gateway: None,
            esl: EslSettings {
                host: "127.0.0.1".into(),
                port: 9,
                password: "x".into(),
            },
        }),
        linphone: linphone_password.map(|password| {
            Arc::new(LinphoneAccount {
                extension: "1001".into(),
                password: password.into(),
                domain: "voiceai.local".into(),
                proxy_host: "10.0.0.5".into(),
                proxy_port: "5062".into(),
                transport: "udp".into(),
                display_name: "Test & Phone".into(),
                did: "0700000000".into(),
            })
        }),
        qr_dir: root.join("qr"),
        auth: Arc::new(AuthSettings::disabled("")),
        http: reqwest::Client::new(),
    })
}

async fn send(
    app: &Router,
    request: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let response = app.clone().oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    (
        parts.status,
        parts.headers,
        to_bytes(body, usize::MAX).await.unwrap().to_vec(),
    )
}

async fn get(app: &Router, path: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    send(app, Request::get(path).body(Body::empty()).unwrap()).await
}

async fn get_json(app: &Router, path: &str) -> (StatusCode, Value) {
    let (status, _, body) = get(app, path).await;
    (status, serde_json::from_slice(&body).unwrap())
}

async fn post_json(app: &Router, path: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::post(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, _, bytes) = send(app, request).await;
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn every_page_renders() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), None);
    for path in [
        "/",
        "/console",
        "/assist",
        "/record",
        "/capture",
        "/train",
        "/create",
        "/brand",
        "/linphone/qr",
    ] {
        let (status, headers, body) = get(&app, path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            headers["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/html"),
            "{path}"
        );
        assert!(
            String::from_utf8(body)
                .unwrap()
                .starts_with("<!DOCTYPE html>"),
            "{path}"
        );
    }
    let (_, _, console) = get(&app, "/console").await;
    let console = String::from_utf8(console).unwrap();
    assert!(console.contains("call-1"));
    assert!(
        console.contains("A person &lt;please&gt;"),
        "caller text is escaped"
    );
}

#[tokio::test]
async fn telephony_apis() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), None);

    let (status, sessions) = get_json(&app, "/api/telephony/sessions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sessions.as_array().unwrap().len(), 1);
    assert_eq!(sessions[0]["session_id"], "call-1");
    assert_eq!(sessions[0]["status"], "transferred");
    assert_eq!(sessions[0]["turn_count"], 2);
    assert_eq!(sessions[0]["has_recording"], true);
    assert_eq!(sessions[0]["is_active"], false);

    let (status, detail) = get_json(&app, "/api/telephony/sessions/call-1").await;
    assert_eq!(
        (status, detail["session_id"].as_str()),
        (StatusCode::OK, Some("call-1"))
    );
    let (status, missing) = get_json(&app, "/api/telephony/sessions/nope").await;
    assert_eq!(
        (status, missing),
        (
            StatusCode::NOT_FOUND,
            json!({"detail": "Session not found"})
        )
    );

    // No API key: the summary falls back to the rule-based analysis and is saved.
    let (status, summary) = get_json(&app, "/api/telephony/sessions/call-1/summary").await;
    assert_eq!(status, StatusCode::OK, "{summary}");
    assert_eq!(summary["analysis"]["status"], "ready");
    assert_eq!(summary["analysis"]["outcome"], "transferred");
    assert!(!summary["content"]["artifacts"]
        .as_array()
        .unwrap()
        .is_empty());
    let (status, _, page) = get(&app, "/summary/call-1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8(page).unwrap().contains("call-1"));
    let (status, _, _) = get(&app, "/summary/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, recent) = get_json(&app, "/api/telephony/recent?limit=5").await;
    assert_eq!((status, recent.is_array()), (StatusCode::OK, true));
}

#[tokio::test]
async fn recording_supports_range_requests() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), None);
    let request = Request::get("/api/telephony/sessions/call-1/recording")
        .header("range", "bytes=0-9")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body.len(), 10);
    assert_eq!(headers["content-type"], "audio/wav");
    assert_eq!(headers["content-range"], "bytes 0-9/400");
    assert_eq!(
        headers["content-disposition"],
        "attachment; filename=\"chan-1-call.wav\""
    );
    assert_eq!(headers["cache-control"], "private, max-age=60");
    let (status, _, _) = get(&app, "/api/telephony/sessions/nope/recording").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn click_to_call_dry_run_and_validation() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), None);
    let (status, ok) = post_json(
        &app,
        "/api/telephony/originate",
        json!({"to": "+61 412 345 678", "dry_run": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (ok["dest"].as_str(), ok["dry_run"].as_bool()),
        (Some("0412345678"), Some(true))
    );
    let (status, bad) = post_json(
        &app,
        "/api/telephony/originate",
        json!({"to": "hello", "dry_run": true}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(bad["detail"]["error"], "bad_dest");
    let (status, missing) = post_json(&app, "/api/telephony/originate", json!({})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(missing["detail"][0]["loc"], json!(["body", "to"]));
}

#[tokio::test]
async fn brand_apis() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), None);
    let (_, clients) = get_json(&app, "/api/brand/clients").await;
    assert_eq!(clients["default_client_id"], "acme");
    assert_eq!(clients["clients"][0]["company"], "Acme Pty Ltd");
    assert_eq!(clients["kinds"]["closed"], "Closed period");
    assert_eq!(clients["languages"].as_array().unwrap().len(), 24);
    assert_eq!(
        (clients["speed_min"].as_f64(), clients["speed_max"].as_f64()),
        (Some(0.7), Some(1.5))
    );

    let (_, template) = get_json(
        &app,
        "/api/brand/acme/template?kind=closed&starts_on=2026-12-24&ends_on=2027-01-02",
    )
    .await;
    assert!(template["script"]
        .as_str()
        .unwrap()
        .contains("24 December 2026"));
    assert_eq!(template["title"], "Closed period 2026-12-24 to 2027-01-02");

    // xAI is unreachable: stock voices fall back and the error is reported, not raised.
    let (status, voices) = get_json(&app, "/api/brand/acme/voices").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!voices["stock"].as_array().unwrap().is_empty());
    assert!(!voices["xai_error"].as_str().unwrap().is_empty());

    let (status, registered) = post_json(
        &app,
        "/api/brand/acme/voices",
        json!({"voice_id": "voice-123", "name": "Kim"}),
    )
    .await;
    assert_eq!(
        (status, registered["voice"]["voice_id"].as_str()),
        (StatusCode::OK, Some("voice-123"))
    );
    let (status, missing) = post_json(&app, "/api/brand/acme/voices", json!({"name": "x"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{missing}");

    let (status, unknown) = get_json(&app, "/api/brand/nope/messages").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(unknown["detail"].is_string());
    let (status, bad) = post_json(
        &app,
        "/api/brand/acme/messages",
        json!({"script": "", "voice_id": "eve"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    let (status, no_key) = post_json(
        &app,
        "/api/brand/acme/messages",
        json!({"script": "We are closed.", "voice_id": "eve"}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{no_key}");
    let (_, messages) = get_json(&app, "/api/brand/acme/messages").await;
    assert_eq!(messages, json!({"messages": []}));
}

#[tokio::test]
async fn device_upload_rejects_non_wav() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), None);
    let body = "--b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"note.txt\"\r\nContent-Type: text/plain\r\n\r\nhi\r\n--b--\r\n";
    let request = Request::post("/api/device/upload")
        .header("content-type", "multipart/form-data; boundary=b")
        .body(Body::from(body))
        .unwrap();
    let (status, _, bytes) = send(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["detail"],
        "Only WAV uploads are supported in this MVP"
    );
}

#[tokio::test]
async fn linphone_provisioning_needs_a_configured_password() {
    let dir = tempfile::tempdir().unwrap();
    let off = app(dir.path(), None);
    let (status, _, _) = get(&off, "/api/linphone/provision/1001").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "no password configured means no provisioning"
    );

    let on = app(dir.path(), Some("s3cret<&"));
    let (status, headers, body) = get(&on, "/api/linphone/provision/1001").await;
    let xml = String::from_utf8(body).unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/xml");
    assert_eq!(headers["cache-control"], "no-store");
    assert!(xml.contains("<entry name=\"passwd\">s3cret&lt;&amp;</entry>"));
    assert!(
        xml.contains("<entry name=\"reg_proxy\">&lt;sip:10.0.0.5:5062;transport=udp&gt;</entry>")
    );
    assert!(xml.contains("<entry name=\"display_name\">Test &amp; Phone</entry>"));
    let (_, _, page) = get(&on, "/linphone/qr").await;
    assert!(
        !String::from_utf8(page).unwrap().contains("s3cret"),
        "the QR page never shows the password"
    );
    let (status, _, _) = get(&on, "/api/linphone/provision/1000").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = get(&on, "/linphone/qr/image/bogus").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn health_check() {
    let dir = tempfile::tempdir().unwrap();
    let (status, body) = get_json(&app(dir.path(), None), "/healthz").await;
    assert_eq!(
        (status, body),
        (StatusCode::OK, json!({"ok": true, "auth": false}))
    );
}

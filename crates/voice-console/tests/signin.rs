//! Microsoft sign-in end to end against a mock identity provider.
use axum::{
    body::{to_bytes, Body},
    http::{HeaderMap, Request, StatusCode},
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine as _};
use serde_json::{json, Value};
use std::{collections::HashMap, path::Path, sync::Arc};
use tower::ServiceExt;
use voice_brand::{BrandSettings, BrandStore};
use voice_console::{
    app::{router, AppState},
    auth::AuthSettings,
};
use voice_core::{
    config::{EslSettings, XaiSettings},
    originate::OriginateEnv,
    xai::XaiRest,
};
use voice_sessions::{service::SessionService, settings::ConsoleSettings};
use wiremock::{
    matchers::{body_string_contains, method, path},
    Mock, MockServer, ResponseTemplate,
};

fn app(root: &Path, authority: &str, base_path: &str) -> Router {
    let mut settings = ConsoleSettings::for_dir(root);
    settings.base_path = base_path.into();
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
        default_client_id: "default".into(),
    };
    let mut auth = AuthSettings::disabled(base_path);
    auth.enabled = true;
    auth.tenant_id = "tenant-1".into();
    auth.client_id = "client-1".into();
    auth.client_secret = "shh".into();
    auth.authority = authority.into();
    auth.allowed_domains = ["neweraits.com".to_string()].into();
    router(AppState {
        sessions: SessionService::new(settings, xai.clone()),
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
        linphone: None,
        qr_dir: root.join("qr"),
        auth: Arc::new(auth),
        http: reqwest::Client::new(),
    })
}

async fn get(app: &Router, uri: &str, cookie: Option<&str>) -> (StatusCode, HeaderMap, String) {
    let mut request = Request::get(uri).header("host", "console.test");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    let text = String::from_utf8(to_bytes(body, usize::MAX).await.unwrap().to_vec()).unwrap();
    (parts.status, parts.headers, text)
}

fn cookie_from(headers: &HeaderMap) -> String {
    headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

fn id_token(claims: Value) -> String {
    format!(
        "{}.{}.sig",
        B64.encode(br#"{"alg":"RS256"}"#),
        B64.encode(claims.to_string())
    )
}

/// Start sign-in and return (session cookie, authorize query parameters).
async fn begin(app: &Router, next: &str) -> (String, HashMap<String, String>) {
    let (status, headers, _) = get(app, &format!("/auth/login?next={next}"), None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let location = url::Url::parse(headers["location"].to_str().unwrap()).unwrap();
    assert!(location.path().ends_with("/oauth2/v2.0/authorize"));
    (
        cookie_from(&headers),
        location.query_pairs().into_owned().collect(),
    )
}

async fn mock_token(idp: &MockServer, claims: Value) {
    idp.reset().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/v2.0/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=the-code"))
        .and(body_string_contains("client_secret=shh"))
        .and(body_string_contains("code_verifier="))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id_token": id_token(claims)})),
        )
        .mount(idp)
        .await;
}

fn claims(nonce: &str, email: &str) -> Value {
    json!({"aud": "client-1", "nonce": nonce, "exp": chrono::Utc::now().timestamp() + 600,
           "tid": "tenant-1", "oid": "o-1", "name": "Kim Staff", "preferred_username": email})
}

#[tokio::test]
async fn signed_out_visitors_are_gated() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), "http://idp.invalid", "");
    let (status, headers, _) = get(&app, "/console?tab=1", None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers["location"], "/auth/login?next=%2Fconsole%3Ftab%3D1");
    let (status, _, body) = get(&app, "/api/telephony/sessions", None).await;
    assert_eq!(
        (status, body.as_str()),
        (
            StatusCode::UNAUTHORIZED,
            r#"{"detail":"Not authenticated"}"#
        )
    );
    let (status, _, body) = get(&app, "/healthz", None).await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::OK, r#"{"ok":true,"auth":true}"#)
    );
    let (status, _, _) = get(&app, "/", Some("voice_in_session=forged.cookie")).await;
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "a forged cookie is not a session"
    );
}

#[tokio::test]
async fn full_sign_in_then_sign_out() {
    let dir = tempfile::tempdir().unwrap();
    let idp = MockServer::start().await;
    let app = app(dir.path(), &idp.uri(), "");

    let (flow_cookie, params) = begin(&app, "/capture").await;
    assert_eq!(params["client_id"], "client-1");
    assert_eq!(params["response_type"], "code");
    assert_eq!(params["redirect_uri"], "http://console.test/auth/callback");
    assert_eq!(params["scope"], "openid profile offline_access");
    assert_eq!(params["code_challenge_method"], "S256");
    assert!(params["code_challenge"].len() >= 43);

    mock_token(&idp, claims(&params["nonce"], "Kim@NewEraITs.com")).await;
    let callback = format!("/auth/callback?code=the-code&state={}", params["state"]);
    let (status, headers, _) = get(&app, &callback, Some(&flow_cookie)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers["location"], "/capture");
    let set_cookie = headers["set-cookie"].to_str().unwrap();
    assert!(
        set_cookie.contains("HttpOnly")
            && set_cookie.contains("SameSite=Lax")
            && set_cookie.contains("Max-Age=43200")
    );
    let session = cookie_from(&headers);

    let (status, _, page) = get(&app, "/", Some(&session)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        page.contains("Kim Staff") && page.contains("kim@neweraits.com"),
        "the signed-in user is shown"
    );
    let (status, _, _) = get(&app, "/api/telephony/sessions", Some(&session)).await;
    assert_eq!(status, StatusCode::OK);

    let (status, headers, _) = get(&app, "/auth/logout", Some(&session)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let logout = headers["location"].to_str().unwrap();
    assert!(logout.starts_with(&format!(
        "{}/oauth2/v2.0/logout?post_logout_redirect_uri=",
        idp.uri()
    )));
    assert!(headers["set-cookie"]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
}

#[tokio::test]
async fn rejected_sign_ins() {
    let dir = tempfile::tempdir().unwrap();
    let idp = MockServer::start().await;
    let app = app(dir.path(), &idp.uri(), "");

    // Wrong state: the callback does not belong to this browser's sign-in.
    let (cookie, params) = begin(&app, "/").await;
    mock_token(&idp, claims(&params["nonce"], "kim@neweraits.com")).await;
    let (status, _, page) = get(
        &app,
        "/auth/callback?code=the-code&state=wrong",
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(page.contains("Sign-in session expired. Try again."));
    assert!(
        idp.received_requests().await.unwrap().is_empty(),
        "no token exchange for a bad state"
    );

    // No sign-in in progress.
    let (status, _, _) = get(&app, "/auth/callback?code=the-code&state=x", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Microsoft reports an error.
    let (status, _, page) = get(
        &app,
        "/auth/callback?error=access_denied&error_description=User+cancelled",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(page.contains("User cancelled"));

    // Token for a different nonce (replay).
    let (cookie, params) = begin(&app, "/").await;
    mock_token(&idp, claims("someone-elses-nonce", "kim@neweraits.com")).await;
    let callback = format!("/auth/callback?code=the-code&state={}", params["state"]);
    let (status, _, _) = get(&app, &callback, Some(&cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // An account outside the allowed domain.
    let (cookie, params) = begin(&app, "/").await;
    mock_token(&idp, claims(&params["nonce"], "stranger@example.com")).await;
    let callback = format!("/auth/callback?code=the-code&state={}", params["state"]);
    let (status, headers, page) = get(&app, &callback, Some(&cookie)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(page.contains("Your Microsoft account is not allowed to use this ops portal."));
    assert!(headers["set-cookie"]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));

    // Open redirects are refused.
    let (_, headers, _) = get(&app, "/auth/login?next=https://evil.example/", None).await;
    assert!(headers.contains_key("location"));
    let (cookie, params) = begin(&app, "https://evil.example/").await;
    mock_token(&idp, claims(&params["nonce"], "kim@neweraits.com")).await;
    let callback = format!("/auth/callback?code=the-code&state={}", params["state"]);
    let (_, headers, _) = get(&app, &callback, Some(&cookie)).await;
    assert_eq!(headers["location"], "/");
}

#[tokio::test]
async fn base_path_is_applied_behind_a_proxy() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), "http://idp.invalid", "/ops");
    let (status, headers, _) = get(&app, "/console", None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers["location"], "/ops/auth/login?next=%2Fops%2Fconsole");
    let (_, headers, _) = get(&app, "/auth/login", None).await;
    let location = url::Url::parse(headers["location"].to_str().unwrap()).unwrap();
    let params: HashMap<String, String> = location.query_pairs().into_owned().collect();
    assert_eq!(
        params["redirect_uri"],
        "http://console.test/ops/auth/callback"
    );
    assert!(headers["set-cookie"]
        .to_str()
        .unwrap()
        .contains("Path=/ops;"));
}

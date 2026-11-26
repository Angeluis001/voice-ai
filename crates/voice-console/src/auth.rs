//! Microsoft Entra ID sign-in for the console (authorization-code flow with PKCE).
//!
//! Off unless the Azure AD variables are set (or `VOICE_IN_AUTH_ENABLED=true`), so a
//! local console stays open. When on, every route needs a signed-in user except the
//! health check, the sign-in routes and Linphone provisioning.
//!
//! The session lives in a signed cookie (HMAC-SHA256); it is not encrypted and holds
//! only the user's name/email and, during sign-in, the flow's state, nonce and PKCE
//! verifier. The ID token is taken directly from Microsoft's token endpoint over TLS
//! and its claims (audience, nonce, expiry, tenant) are checked.
use crate::{
    app::AppState,
    ui::{self, UiContext, UiUser},
};
use axum::{
    extract::{Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine as _};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

pub const SESSION_COOKIE: &str = "voice_in_session";
const SESSION_MAX_AGE: i64 = 12 * 60 * 60;
const PUBLIC_EXACT: [&str; 4] = ["/healthz", "/auth/login", "/auth/callback", "/auth/logout"];
const PUBLIC_PREFIXES: [&str; 1] = ["/api/linphone/provision"];

tokio::task_local! {
    /// The signed-in user for the request being handled.
    static CURRENT_USER: Option<UiUser>;
}

pub fn current_user() -> Option<UiUser> {
    CURRENT_USER.try_with(Clone::clone).ok().flatten()
}

#[derive(Debug, Clone)]
pub struct AuthSettings {
    pub enabled: bool,
    pub tenant_id: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub session_secret: String,
    pub session_secure: bool,
    pub allowed_emails: HashSet<String>,
    pub allowed_domains: HashSet<String>,
    /// `https://login.microsoftonline.com/<tenant>` unless overridden.
    pub authority: String,
    /// URL prefix behind a reverse proxy (for example `/ops`).
    pub base_path: String,
}

fn random_token() -> String {
    let bytes: Vec<u8> = [uuid::Uuid::new_v4(), uuid::Uuid::new_v4()]
        .iter()
        .flat_map(|u| *u.as_bytes())
        .collect();
    B64.encode(bytes)
}

impl AuthSettings {
    pub fn disabled(base_path: &str) -> Self {
        Self {
            enabled: false,
            tenant_id: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            redirect_uri: String::new(),
            session_secret: random_token(),
            session_secure: false,
            allowed_emails: HashSet::new(),
            allowed_domains: HashSet::new(),
            authority: String::new(),
            base_path: base_path.to_string(),
        }
    }

    pub fn from_env(base_path: &str) -> Result<Self, String> {
        let var = |name: &str| std::env::var(name).unwrap_or_default().trim().to_string();
        let (tenant_id, client_id, client_secret) = (
            var("AZURE_AD_TENANT_ID"),
            var("AZURE_AD_CLIENT_ID"),
            var("AZURE_AD_CLIENT_SECRET"),
        );
        let redirect_uri = var("AZURE_AD_REDIRECT_URI");
        let complete = !tenant_id.is_empty() && !client_id.is_empty() && !client_secret.is_empty();
        let enabled = match var("VOICE_IN_AUTH_ENABLED").to_lowercase().as_str() {
            "0" | "false" | "no" | "off" => false,
            "1" | "true" | "yes" | "on" => true,
            _ => complete,
        };
        if enabled && !complete {
            return Err("Voice IN Microsoft login is enabled but AZURE_AD_TENANT_ID, AZURE_AD_CLIENT_ID, or AZURE_AD_CLIENT_SECRET is missing.".into());
        }
        let mut session_secret = var("VOICE_IN_SESSION_SECRET");
        if enabled && session_secret.is_empty() {
            return Err(
                "VOICE_IN_SESSION_SECRET is required when Microsoft login is enabled.".into(),
            );
        }
        if session_secret.is_empty() {
            session_secret = random_token();
        }
        let session_secure = match var("VOICE_IN_SESSION_SECURE").to_lowercase().as_str() {
            "" => enabled && redirect_uri.starts_with("https://"),
            value => matches!(value, "1" | "true" | "yes" | "on"),
        };
        let list = |name: &str| -> HashSet<String> {
            var(name)
                .split(',')
                .map(|item| {
                    item.trim()
                        .to_lowercase()
                        .trim_start_matches('@')
                        .to_string()
                })
                .filter(|item| !item.is_empty())
                .collect()
        };
        let authority = match var("AZURE_AD_AUTHORITY") {
            custom if !custom.is_empty() => custom.trim_end_matches('/').to_string(),
            _ => format!("https://login.microsoftonline.com/{tenant_id}"),
        };
        Ok(Self {
            enabled,
            allowed_emails: list("AZURE_AD_ALLOWED_EMAILS"),
            allowed_domains: list("AZURE_AD_ALLOWED_DOMAINS"),
            tenant_id,
            client_id,
            client_secret,
            redirect_uri,
            session_secret,
            session_secure,
            authority,
            base_path: base_path.to_string(),
        })
    }

    fn path(&self, path: &str) -> String {
        UiContext {
            base_path: self.base_path.clone(),
            user: None,
            auth_enabled: self.enabled,
        }
        .path(path)
    }

    pub fn ui_context(&self) -> UiContext {
        UiContext {
            base_path: self.base_path.clone(),
            user: current_user(),
            auth_enabled: self.enabled,
        }
    }

    /// Where to send the user after sign-in: only local paths, never the auth routes.
    fn safe_next(&self, value: Option<&str>) -> String {
        let fallback = self.path("/");
        let Some(value) = value.filter(|v| !v.is_empty() && *v != "/") else {
            return fallback;
        };
        if !value.starts_with('/')
            || value.starts_with("//")
            || value.contains('\\')
            || value.contains("://")
        {
            return fallback;
        }
        if value.to_lowercase().contains("/auth/") {
            return fallback;
        }
        let base = &self.base_path;
        if !base.is_empty() && !value.starts_with(&format!("{base}/")) && value != base {
            return self.path(value);
        }
        value.to_string()
    }

    fn user_allowed(&self, user: &SessionUser) -> bool {
        if !user.tid.is_empty() && user.tid != self.tenant_id {
            return false;
        }
        if !self.allowed_emails.is_empty() && !self.allowed_emails.contains(&user.email) {
            return false;
        }
        if !self.allowed_domains.is_empty() {
            let domain = user.email.rsplit_once('@').map(|(_, d)| d).unwrap_or("");
            if !self.allowed_domains.contains(domain) {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionUser {
    pub oid: String,
    pub name: String,
    pub email: String,
    pub tid: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AuthFlow {
    state: String,
    nonce: String,
    verifier: String,
    redirect_uri: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Session {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user: Option<SessionUser>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    flow: Option<AuthFlow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next: Option<String>,
    #[serde(default)]
    exp: i64,
}

fn sign(secret: &str, payload: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(payload.as_bytes());
    B64.encode(mac.finalize().into_bytes())
}

fn encode_session(settings: &AuthSettings, session: &Session) -> String {
    let mut session = session.clone();
    session.exp = chrono::Utc::now().timestamp() + SESSION_MAX_AGE;
    let payload = B64.encode(serde_json::to_vec(&session).unwrap_or_default());
    format!("{payload}.{}", sign(&settings.session_secret, &payload))
}

fn decode_session(settings: &AuthSettings, cookie: &str) -> Option<Session> {
    let (payload, signature) = cookie.rsplit_once('.')?;
    let mut mac = Hmac::<Sha256>::new_from_slice(settings.session_secret.as_bytes()).ok()?;
    mac.update(payload.as_bytes());
    mac.verify_slice(&B64.decode(signature).ok()?).ok()?;
    let session: Session = serde_json::from_slice(&B64.decode(payload).ok()?).ok()?;
    (session.exp > chrono::Utc::now().timestamp()).then_some(session)
}

fn read_session(settings: &AuthSettings, headers: &HeaderMap) -> Session {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .and_then(|(_, value)| decode_session(settings, value))
        .unwrap_or_default()
}

fn session_cookie(settings: &AuthSettings, session: Option<&Session>) -> HeaderValue {
    let path = if settings.base_path.is_empty() {
        "/"
    } else {
        &settings.base_path
    };
    let secure = if settings.session_secure {
        "; Secure"
    } else {
        ""
    };
    let cookie = match session {
        Some(session) => format!(
            "{SESSION_COOKIE}={}; Path={path}; Max-Age={SESSION_MAX_AGE}; HttpOnly; SameSite=Lax{secure}",
            encode_session(settings, session)
        ),
        None => format!("{SESSION_COOKIE}=; Path={path}; Max-Age=0; HttpOnly; SameSite=Lax{secure}"),
    };
    HeaderValue::from_str(&cookie).unwrap_or(HeaderValue::from_static(""))
}

fn with_cookie(
    mut response: Response,
    settings: &AuthSettings,
    session: Option<&Session>,
) -> Response {
    response
        .headers_mut()
        .append(header::SET_COOKIE, session_cookie(settings, session));
    response
}

fn public_path(path: &str) -> bool {
    let normalized = match path.trim_end_matches('/') {
        "" => "/",
        other => other,
    };
    PUBLIC_EXACT.contains(&normalized)
        || PUBLIC_PREFIXES
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
}

fn wants_json(request: &Request) -> bool {
    if request.uri().path().starts_with("/api/") {
        return true;
    }
    let accept = request
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    if accept.contains("text/event-stream")
        || (accept.contains("application/json") && !accept.contains("text/html"))
    {
        return true;
    }
    !matches!(request.method().as_str(), "GET" | "HEAD")
}

fn encode_query(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

/// Gate every request: public paths and signed-in users pass, everyone else is sent
/// to sign in (pages) or told they are not authenticated (APIs).
pub async fn gate(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let settings = &state.auth;
    let user = if settings.enabled {
        read_session(settings, request.headers()).user
    } else {
        None
    };
    let ui_user = user.as_ref().map(|u| UiUser {
        name: u.name.clone(),
        email: u.email.clone(),
    });
    if !settings.enabled || public_path(request.uri().path()) || user.is_some() {
        return CURRENT_USER.scope(ui_user, next.run(request)).await;
    }
    if wants_json(&request) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"detail": "Not authenticated"})),
        )
            .into_response();
    }
    let raw = request
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let next_url = settings.safe_next(Some(&settings.path(raw)));
    let target = format!(
        "{}?{}",
        settings.path("/auth/login"),
        encode_query(&[("next", &next_url)])
    );
    Redirect::to(&target).into_response()
}

fn request_origin(headers: &HeaderMap) -> String {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let scheme = match get("x-forwarded-proto") {
        proto if !proto.is_empty() => proto,
        _ => "http".into(),
    };
    format!("{scheme}://{}", get("host"))
}

fn callback_uri(settings: &AuthSettings, headers: &HeaderMap) -> String {
    if !settings.redirect_uri.is_empty() {
        return settings.redirect_uri.clone();
    }
    format!(
        "{}{}",
        request_origin(headers),
        settings.path("/auth/callback")
    )
}

#[derive(Deserialize)]
pub struct LoginQuery {
    next: Option<String>,
}

pub async fn login(
    State(state): State<AppState>,
    Query(query): Query<LoginQuery>,
    headers: HeaderMap,
) -> Response {
    let settings = &state.auth;
    if !settings.enabled {
        return Redirect::to(&settings.path("/")).into_response();
    }
    let flow = AuthFlow {
        state: random_token(),
        nonce: random_token(),
        verifier: format!("{}{}", random_token(), random_token()),
        redirect_uri: callback_uri(settings, &headers),
    };
    let challenge = B64.encode(Sha256::digest(flow.verifier.as_bytes()));
    let authorize = format!(
        "{}/oauth2/v2.0/authorize?{}",
        settings.authority,
        encode_query(&[
            ("client_id", &settings.client_id),
            ("response_type", "code"),
            ("redirect_uri", &flow.redirect_uri),
            ("response_mode", "query"),
            ("scope", "openid profile offline_access"),
            ("state", &flow.state),
            ("nonce", &flow.nonce),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ])
    );
    let session = Session {
        flow: Some(flow),
        next: Some(settings.safe_next(query.next.as_deref())),
        ..Default::default()
    };
    with_cookie(
        Redirect::to(&authorize).into_response(),
        settings,
        Some(&session),
    )
}

fn login_page(settings: &AuthSettings, status: StatusCode, error: &str) -> Response {
    (
        status,
        Html(ui::render_login(&settings.ui_context(), error)),
    )
        .into_response()
}

fn id_token_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&B64.decode(payload.trim_end_matches('=')).ok()?).ok()
}

pub async fn callback(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let settings = &state.auth;
    if !settings.enabled {
        return Redirect::to(&settings.path("/")).into_response();
    }
    let param = |name: &str| {
        query
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    };
    if let Some(error) = param("error") {
        return login_page(
            settings,
            StatusCode::UNAUTHORIZED,
            param("error_description").unwrap_or(error),
        );
    }
    let session = read_session(settings, &headers);
    let next = settings.safe_next(session.next.as_deref());
    let expired = |message: &str| {
        with_cookie(
            login_page(settings, StatusCode::UNAUTHORIZED, message),
            settings,
            None,
        )
    };
    let Some(flow) = session.flow else {
        return expired("Sign-in session expired. Try again.");
    };
    let (Some(code), Some(returned_state)) = (param("code"), param("state")) else {
        return expired("Sign-in failed.");
    };
    if returned_state != flow.state {
        return expired("Sign-in session expired. Try again.");
    }

    let form = encode_query(&[
        ("client_id", &settings.client_id),
        ("client_secret", &settings.client_secret),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", &flow.redirect_uri),
        ("code_verifier", &flow.verifier),
        ("scope", "openid profile offline_access"),
    ]);
    let exchange = async {
        let response = state
            .http
            .post(format!("{}/oauth2/v2.0/token", settings.authority))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        response.json::<Value>().await.map_err(|e| e.to_string())
    };
    let result = match exchange.await {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!("Microsoft token exchange failed: {error}");
            return expired("Sign-in failed.");
        }
    };
    if !result["error"].is_null() {
        let description = result["error_description"]
            .as_str()
            .or(result["error"].as_str())
            .unwrap_or("Sign-in failed.");
        tracing::warn!("Microsoft token exchange failed: {description}");
        return expired(description);
    }
    let claims = result["id_token"]
        .as_str()
        .and_then(id_token_claims)
        .unwrap_or(Value::Null);
    let text = |key: &str| claims[key].as_str().unwrap_or_default().to_string();
    let audience_ok = claims["aud"].as_str() == Some(settings.client_id.as_str());
    let fresh = claims["exp"]
        .as_i64()
        .map(|exp| exp > chrono::Utc::now().timestamp())
        .unwrap_or(false);
    if !audience_ok || !fresh || text("nonce") != flow.nonce {
        tracing::warn!("Microsoft ID token failed validation");
        return expired("Sign-in failed.");
    }
    let email = ["preferred_username", "email", "upn"]
        .iter()
        .map(|k| text(k))
        .find(|v| !v.is_empty())
        .unwrap_or_default();
    let user = SessionUser {
        oid: text("oid"),
        name: text("name"),
        email: email.trim().to_lowercase(),
        tid: text("tid"),
    };
    if user.email.is_empty() || !settings.user_allowed(&user) {
        tracing::warn!(
            "Microsoft sign-in rejected for {}",
            if user.email.is_empty() {
                &user.oid
            } else {
                &user.email
            }
        );
        let page = login_page(
            settings,
            StatusCode::FORBIDDEN,
            "Your Microsoft account is not allowed to use this ops portal.",
        );
        return with_cookie(page, settings, None);
    }
    let session = Session {
        user: Some(user),
        ..Default::default()
    };
    with_cookie(
        Redirect::to(&next).into_response(),
        settings,
        Some(&session),
    )
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let settings = &state.auth;
    if !settings.enabled {
        return with_cookie(
            Redirect::to(&settings.path("/")).into_response(),
            settings,
            None,
        );
    }
    let post_logout = match settings.redirect_uri.strip_suffix("/auth/callback") {
        Some(root) => format!("{root}/"),
        None => format!("{}{}", request_origin(&headers), settings.path("/")),
    };
    let target = format!(
        "{}/oauth2/v2.0/logout?{}",
        settings.authority,
        encode_query(&[("post_logout_redirect_uri", &post_logout)])
    );
    with_cookie(Redirect::to(&target).into_response(), settings, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(base: &str) -> AuthSettings {
        let mut s = AuthSettings::disabled(base);
        s.enabled = true;
        s.tenant_id = "tenant-1".into();
        s
    }

    #[test]
    fn next_url_is_always_local() {
        let s = settings("");
        assert_eq!(s.safe_next(None), "/");
        assert_eq!(s.safe_next(Some("/console?x=1")), "/console?x=1");
        for bad in [
            "https://evil.example",
            "//evil.example",
            "/a\\b",
            "/x?u=http://e",
            "/auth/login",
            "console",
        ] {
            assert_eq!(s.safe_next(Some(bad)), "/", "{bad}");
        }
        let ops = settings("/ops");
        assert_eq!(ops.safe_next(Some("/console")), "/ops/console");
        assert_eq!(ops.safe_next(Some("/ops/console")), "/ops/console");
        assert_eq!(ops.safe_next(None), "/ops/");
    }

    #[test]
    fn session_cookie_round_trip_and_tamper_detection() {
        let s = settings("");
        let session = Session {
            user: Some(SessionUser {
                email: "a@b.c".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cookie = encode_session(&s, &session);
        assert_eq!(
            decode_session(&s, &cookie).unwrap().user.unwrap().email,
            "a@b.c"
        );
        let (payload, signature) = cookie.rsplit_once('.').unwrap();
        let forged = B64.encode(
            br#"{"user":{"oid":"","name":"","email":"evil@b.c","tid":""},"exp":99999999999}"#,
        );
        assert!(decode_session(&s, &format!("{forged}.{signature}")).is_none());
        assert!(decode_session(&s, payload).is_none());
        let other = settings("");
        assert!(
            decode_session(&other, &cookie).is_none(),
            "a different secret rejects the cookie"
        );
    }

    #[test]
    fn allow_lists() {
        let mut s = settings("");
        let user = |email: &str, tid: &str| SessionUser {
            email: email.into(),
            tid: tid.into(),
            ..Default::default()
        };
        assert!(s.user_allowed(&user("a@x.com", "tenant-1")));
        assert!(!s.user_allowed(&user("a@x.com", "other-tenant")));
        s.allowed_domains = ["x.com".to_string()].into();
        assert!(s.user_allowed(&user("a@x.com", "")));
        assert!(!s.user_allowed(&user("a@y.com", "")));
        s.allowed_emails = ["boss@x.com".to_string()].into();
        assert!(!s.user_allowed(&user("a@x.com", "")));
        assert!(s.user_allowed(&user("boss@x.com", "")));
    }

    #[test]
    fn public_paths() {
        for open in [
            "/healthz",
            "/auth/login",
            "/auth/callback/",
            "/api/linphone/provision/1001",
        ] {
            assert!(public_path(open), "{open}");
        }
        for closed in ["/", "/console", "/api/telephony/sessions", "/linphone/qr"] {
            assert!(!public_path(closed), "{closed}");
        }
    }
}

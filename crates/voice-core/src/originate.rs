//! Click-to-call: ring the operator's phone (A-leg), then bridge out through the
//! carrier gateway (B-leg), using the FreeSWITCH event socket.
//!
//! Dial rules (operator endpoint, caller ID, gateway, number normalisation, do-not-call
//! list) come from a rules JSON file when one is found, otherwise from defaults.
use crate::{
    config::EslSettings,
    esl::EslClient,
    util::{js_str, truthy},
    Doc,
};
use regex::Regex;
use serde_json::{json, Value};
use std::{env, path::PathBuf, time::Duration};

const RULES_FILE: &str = "persona.contact.voice.dial.v1.rules.json";
const DEFAULT_OPERATOR: &str = "sofia/local_device/1001@voiceai.local";
const DEFAULT_CLI: &str = "0734952902";
const DEFAULT_GATEWAY: &str = "sofia/gateway/telair";

fn var(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[derive(Debug, Clone, Default)]
pub struct OriginateRequest {
    pub to: String,
    pub stable_entity_id: String,
    pub related_matter_seid: String,
    pub operator_id: String,
    pub command_id: String,
    pub dry_run: bool,
}

/// Environment overrides and where to reach FreeSWITCH.
#[derive(Debug, Clone)]
pub struct OriginateEnv {
    pub rules_path: Option<PathBuf>,
    pub operator_endpoint: Option<String>,
    pub outbound_cli: Option<String>,
    pub outbound_cli_name: Option<String>,
    pub gateway: Option<String>,
    pub esl: EslSettings,
}

fn read_esl_password() -> String {
    if let Some(password) = var("FREESWITCH_ESL_PASSWORD") {
        return password;
    }
    let conf = "/app/docker/sip-stack/freeswitch/conf/autoload_configs/event_socket.conf.xml";
    if let Ok(text) = std::fs::read_to_string(conf) {
        let pattern = Regex::new(r#"name="password"\s+value="([^"]+)""#).expect("static pattern");
        if let Some(found) = pattern.captures(&text) {
            return found[1].to_string();
        }
    }
    if let Ok(text) = std::fs::read_to_string("/data/pilot/esl.password") {
        return text.trim().to_string();
    }
    // FreeSWITCH's stock event-socket password.
    "ClueCon".to_string()
}

impl OriginateEnv {
    pub fn from_env() -> Self {
        Self {
            rules_path: var("VOICE_DIAL_RULES_PATH").map(PathBuf::from),
            operator_endpoint: var("VOICE_OPERATOR_ENDPOINT"),
            outbound_cli: var("VOICE_OUTBOUND_CLI"),
            outbound_cli_name: var("VOICE_OUTBOUND_CLI_NAME"),
            gateway: var("VOICE_OUTBOUND_GATEWAY"),
            esl: EslSettings {
                host: var("FREESWITCH_ESL_HOST").unwrap_or_else(|| "172.17.0.1".into()),
                port: var("FREESWITCH_ESL_PORT")
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(8021),
                password: read_esl_password(),
            },
        }
    }

    /// Load the dial rules: the first rules file that exists, else built-in defaults.
    pub fn load_dial_rules(&self) -> Doc {
        let candidates = self.rules_path.clone().into_iter().chain([
            PathBuf::from("/app/data/pa").join(RULES_FILE),
            PathBuf::from("data/pa").join(RULES_FILE),
            PathBuf::from("/opt/voice-ai/voice_in_agent").join(RULES_FILE),
        ]);
        for path in candidates {
            if !path.is_file() {
                continue;
            }
            if let Ok(Value::Object(rules)) = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
            {
                return rules;
            }
        }
        let Value::Object(rules) = json!({
            "operator_endpoint": self.operator_endpoint.clone().unwrap_or_else(|| DEFAULT_OPERATOR.into()),
            "outbound_cli": self.outbound_cli.clone().unwrap_or_else(|| DEFAULT_CLI.into()),
            "outbound_cli_name": self.outbound_cli_name.clone().unwrap_or_else(|| "ValOS".into()),
            "gateway": self.gateway.clone().unwrap_or_else(|| DEFAULT_GATEWAY.into()),
            "originate_timeout_seconds": 40,
            "normalize_au": {
                "strip": [" ", "-", "(", ")", "."],
                "plus_61_to_0nsn": true,
                "bare_61_to_0nsn": true,
                "keep_leading_zero": true
            },
            "dnc": []
        }) else {
            unreachable!()
        };
        rules
    }
}

/// Normalise an Australian destination to national format (`0` + 9 digits).
pub fn normalize_au_dest(raw: &str, rules: &Doc) -> String {
    let au = rules.get("normalize_au").and_then(Value::as_object);
    let flag = |key: &str| au.and_then(|a| a.get(key)).map(truthy).unwrap_or(false);
    let strip: Vec<String> = au
        .and_then(|a| a.get("strip"))
        .and_then(Value::as_array)
        .filter(|list| !list.is_empty())
        .map(|list| list.iter().map(|v| js_str(Some(v))).collect())
        .unwrap_or_else(|| {
            [" ", "-", "(", ")", "."]
                .iter()
                .map(|s| s.to_string())
                .collect()
        });
    let mut n = raw.trim().to_string();
    for ch in strip.iter().filter(|c| !c.is_empty()) {
        n = n.replace(ch.as_str(), "");
    }
    if let Some(rest) = n.strip_prefix("00") {
        n = format!("+{rest}");
    }
    if flag("plus_61_to_0nsn") && n.starts_with("+61") {
        n = format!("0{}", &n[3..]);
    } else if flag("bare_61_to_0nsn") && n.starts_with("61") && n.chars().count() >= 11 {
        n = format!("0{}", &n[2..]);
    }
    if flag("keep_leading_zero")
        && !n.starts_with('0')
        && n.len() == 9
        && n.chars().all(|c| c.is_ascii_digit())
    {
        n = format!("0{n}");
    }
    n
}

fn none_if_empty(value: &str) -> Value {
    if value.is_empty() {
        Value::Null
    } else {
        json!(value)
    }
}

/// Place a click-to-call. The result always describes what was (or would be) dialled;
/// `ok` is false with an `error` code when the call was not placed.
pub async fn originate(request: &OriginateRequest, env: &OriginateEnv) -> Value {
    let rules = env.load_dial_rules();
    let dest = normalize_au_dest(&request.to, &rules);
    let valid = Regex::new(r"^[0-9+]{6,16}$").expect("static pattern");
    if !valid.is_match(&dest) {
        return json!({"ok": false, "error": "bad_dest", "to": request.to, "dest": dest});
    }
    let dnc = rules
        .get("dnc")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if dnc
        .iter()
        .any(|entry| normalize_au_dest(&js_str(Some(entry)), &rules) == dest)
    {
        return json!({"ok": false, "error": "dnc", "dest": dest});
    }

    let pick = |over: &Option<String>, key: &str, default: &str| {
        over.clone()
            .or_else(|| Some(js_str(rules.get(key))).filter(|v| !v.is_empty()))
            .unwrap_or_else(|| default.to_string())
    };
    let a_leg = pick(
        &env.operator_endpoint,
        "operator_endpoint",
        DEFAULT_OPERATOR,
    );
    let cli = pick(&env.outbound_cli, "outbound_cli", DEFAULT_CLI);
    let cli_name = pick(&None, "outbound_cli_name", "ValOS");
    let gateway = pick(&env.gateway, "gateway", DEFAULT_GATEWAY);
    let timeout = rules
        .get("originate_timeout_seconds")
        .and_then(Value::as_i64)
        .filter(|t| *t != 0)
        .unwrap_or(40);
    let job_id = uuid::Uuid::new_v4().to_string();
    let command = format!(
        "originate {{ignore_early_media=true,originate_timeout={timeout},origination_caller_id_number={cli},\
origination_caller_id_name={cli_name},hangup_after_bridge=true,origination_uuid={job_id}}}{a_leg} \
&bridge({{origination_caller_id_number={cli},origination_caller_id_name={cli_name}}}{gateway}/{dest})"
    );
    let command_id = if request.command_id.is_empty() {
        "contact.voice.dial.v1"
    } else {
        &request.command_id
    };
    let mut payload = json!({
        "ok": true,
        "command_id": command_id,
        "stable_entity_id": none_if_empty(&request.stable_entity_id),
        "related_matter_seid": none_if_empty(&request.related_matter_seid),
        "operator_id": none_if_empty(&request.operator_id),
        "to": request.to,
        "dest": dest,
        "a_leg": a_leg,
        "cli": cli,
        "session_id": job_id,
        "call_id": job_id,
        "dry_run": request.dry_run,
        "esl": command,
    });
    if request.dry_run {
        payload["score"] = json!("HUMAN");
        payload["message"] = json!("dry_run — not sent to Telair");
        return payload;
    }
    let esl = EslClient::new(env.esl.clone()).with_timeout(Duration::from_secs(8));
    match esl.bgapi(&command).await {
        Err(error) => {
            payload["ok"] = json!(false);
            payload["error"] = json!("esl_failed");
            payload["detail"] = json!(error.to_string());
            payload["score"] = json!("FAIL");
        }
        Ok(reply) => {
            payload["esl_reply"] = json!(reply);
            if reply.contains("-ERR") {
                payload["ok"] = json!(false);
                payload["error"] = json!("originate_rejected");
                payload["score"] = json!("FAIL");
            } else {
                payload["score"] = json!("HUMAN");
                payload["message"] = json!("Answer Linphone 1001 — Telair will dial the Actor.");
            }
        }
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esl;

    fn env(port: u16) -> OriginateEnv {
        OriginateEnv {
            rules_path: None,
            operator_endpoint: None,
            outbound_cli: None,
            outbound_cli_name: None,
            gateway: None,
            esl: EslSettings {
                host: "127.0.0.1".into(),
                port,
                password: "pw".into(),
            },
        }
    }

    fn request(to: &str, dry_run: bool) -> OriginateRequest {
        OriginateRequest {
            to: to.into(),
            dry_run,
            ..Default::default()
        }
    }

    #[test]
    fn australian_numbers_normalise_to_national_format() {
        let rules = env(1).load_dial_rules();
        let n = |raw: &str| normalize_au_dest(raw, &rules);
        assert_eq!(n("+61 7 3495 2900"), "0734952900");
        assert_eq!(n("0061734952900"), "0734952900");
        assert_eq!(n("61734952900"), "0734952900");
        assert_eq!(n("(07) 3495-2900"), "0734952900");
        assert_eq!(n("734952900"), "0734952900");
        assert_eq!(n("0412 345 678"), "0412345678");
        assert_eq!(n("+1 415 555 0100"), "+14155550100");
    }

    #[tokio::test]
    async fn dry_run_builds_the_command_without_touching_freeswitch() {
        let fake = esl::fake::start("pw").await;
        let out = originate(&request("+61 7 3495 2900", true), &env(fake.port)).await;
        assert_eq!(out["ok"], true);
        assert_eq!(out["dest"], "0734952900");
        assert_eq!(out["score"], "HUMAN");
        assert_eq!(out["message"], "dry_run — not sent to Telair");
        assert_eq!(out["command_id"], "contact.voice.dial.v1");
        assert_eq!(out["stable_entity_id"], Value::Null);
        let job = out["session_id"].as_str().unwrap();
        assert_eq!(
            out["esl"].as_str().unwrap(),
            format!(
                "originate {{ignore_early_media=true,originate_timeout=40,origination_caller_id_number=0734952902,\
origination_caller_id_name=ValOS,hangup_after_bridge=true,origination_uuid={job}}}sofia/local_device/1001@voiceai.local \
&bridge({{origination_caller_id_number=0734952902,origination_caller_id_name=ValOS}}sofia/gateway/telair/0734952900)"
            )
        );
        assert!(fake.commands.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn real_run_sends_bgapi_and_reports_the_reply() {
        let fake = esl::fake::start("pw").await;
        let mut e = env(fake.port);
        e.operator_endpoint = Some("user/1000".into());
        e.outbound_cli = Some("0700000000".into());
        let out = originate(&request("0412345678", false), &e).await;
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["esl_reply"], "+OK Job-UUID: job-1");
        assert_eq!(out["a_leg"], "user/1000");
        assert_eq!(out["cli"], "0700000000");
        let sent = fake.commands.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].starts_with("originate {ignore_early_media=true"));
        assert!(sent[0].ends_with("sofia/gateway/telair/0412345678)"));
    }

    #[tokio::test]
    async fn bad_numbers_dnc_and_esl_failures_are_reported() {
        let fake = esl::fake::start("pw").await;
        let bad = originate(&request("call me", false), &env(fake.port)).await;
        assert_eq!(
            (bad["ok"].clone(), bad["error"].clone()),
            (json!(false), json!("bad_dest"))
        );

        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("rules.json");
        std::fs::write(
            &rules,
            json!({"dnc": ["+61 412 345 678"], "normalize_au": {"plus_61_to_0nsn": true}})
                .to_string(),
        )
        .unwrap();
        let mut with_rules = env(fake.port);
        with_rules.rules_path = Some(rules);
        let dnc = originate(&request("0412345678", false), &with_rules).await;
        assert_eq!(dnc["error"], "dnc");

        let mut wrong_password = env(fake.port);
        wrong_password.esl.password = "nope".into();
        let failed = originate(&request("0412345678", false), &wrong_password).await;
        assert_eq!(failed["error"], "esl_failed");
        assert_eq!(failed["score"], "FAIL");
        assert!(fake.commands.lock().unwrap().is_empty());
    }
}

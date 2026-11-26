//! Client packs: `<clients_dir>/<id>/client.json` hold the brand, the live agent
//! instructions (SOP) and the roleplay scenarios for one client.
use crate::{util::js_str, Doc};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub const FALLBACK_SOP_INSTRUCTIONS: &str = "You are the first-line phone receptionist on a live phone call.
Speak naturally and briefly (one or two short sentences when possible). Pause so the caller can interrupt.
- Greet warmly if not yet greeted.
- Learn why they are calling and key facts.
- If they want a person, confirm and say you will transfer them.
- Keep answers suitable for telephony. If the caller speaks while you talk, stop and listen.";

pub const DEFAULT_GREETING: &str = "Good morning, how can I help you today?";

fn fallback_roleplay() -> Value {
    json!({
        "general": {
            "label": "General caller",
            "opening": "Hi, I need some help please.",
            "instructions": "You are a ROLEPLAY CALLER for staff training. Staff is the receptionist. Stay as the caller. Short replies."
        },
        "transfer": {
            "label": "Wants a person",
            "opening": "Can I speak to someone please? I don't want a robot.",
            "instructions": "You are a ROLEPLAY CALLER who wants a human. Keep asking for a person if not transferred. Short replies. Stay as the caller."
        }
    })
}

/// A loaded client pack. The raw JSON is kept so the console can read fields the
/// agent does not use (routing, create templates, the roleplay tree).
#[derive(Debug, Clone)]
pub struct ClientPack {
    pub raw: Doc,
}

/// One roleplay scenario, resolved with fallbacks.
#[derive(Debug, Clone, PartialEq)]
pub struct RoleplayScenario {
    pub label: String,
    pub opening: String,
    pub instructions: String,
    pub track: String,
}

/// Replace `{{company}}` and `{{greeting}}` in pack text.
pub fn expand_template(text: &str, company: &str, greeting: &str) -> String {
    let company = if company.is_empty() {
        "our company"
    } else {
        company
    };
    text.replace("{{company}}", company)
        .replace("{{greeting}}", greeting)
}

impl ClientPack {
    fn section(&self, name: &str) -> Option<&Doc> {
        self.raw.get(name).and_then(Value::as_object)
    }

    fn text(&self, section: &str, key: &str) -> String {
        self.section(section)
            .map(|s| js_str(s.get(key)))
            .unwrap_or_default()
    }

    pub fn id(&self) -> String {
        js_str(self.raw.get("id"))
    }
    pub fn company(&self) -> String {
        self.text("brand", "company")
    }
    pub fn greeting(&self) -> String {
        self.text("brand", "greeting")
    }
    pub fn tts_voice(&self) -> String {
        self.text("brand", "tts_voice")
    }
    pub fn sop_instructions(&self) -> String {
        self.text("ai", "sop_instructions")
    }
    pub fn prompt_profile(&self) -> String {
        self.text("ai", "prompt_profile")
    }
    /// Extension a caller is transferred to when they ask for a person.
    pub fn transfer_extension(&self) -> String {
        let ext = self.text("routing", "transfer_extension");
        if ext.is_empty() {
            "1000".into()
        } else {
            ext
        }
    }

    /// Resolve a roleplay scenario: the requested key, else the pack default, else
    /// the built-in general caller.
    pub fn roleplay(&self, scenario_key: &str) -> RoleplayScenario {
        let fallback = fallback_roleplay();
        let roleplay = self.section("roleplay");
        let scenarios = roleplay
            .and_then(|r| r.get("scenarios"))
            .and_then(Value::as_object)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| fallback.as_object().unwrap());
        let default_key = roleplay
            .map(|r| js_str(r.get("default_scenario")))
            .filter(|k| !k.is_empty())
            .or_else(|| scenarios.keys().next().cloned())
            .unwrap_or_else(|| "general".into());
        let wanted = if scenario_key.is_empty() {
            default_key.as_str()
        } else {
            scenario_key
        };
        let key: String = wanted
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
            .collect();
        let chosen = scenarios
            .get(&key)
            .or_else(|| scenarios.get(&default_key))
            .unwrap_or(&fallback["general"]);
        let get = |k: &str| js_str(chosen.get(k));
        RoleplayScenario {
            label: get("label"),
            opening: get("opening"),
            instructions: get("instructions"),
            track: get("track"),
        }
    }

    /// System prompt for the side call that labels intent and extracts slots.
    pub fn intent_prompt(&self) -> String {
        let company = self.company();
        let company = if company.is_empty() {
            "the company"
        } else {
            &company
        };
        format!(
            "You label intent and extract slots for a live phone call at {company}.
Return strict JSON only with:
- intent: short snake_case label (e.g. provide_details, ask_fees, request_transfer, greeting, unsupported_request)
- slots: {{
    debt_type: string|null,
    amount: string|null,
    staff_name: string|null,
    reference: string|null,
    escalation_offered: boolean,
    action: continue|transfer|callback|decline_debt_type|complete
  }}

Policy:
- action=transfer only if caller clearly wants a person transfer now.
- action=callback only if caller accepts/requests a callback to end.
- action=complete when the enquiry is finished.
- Prefer action=continue otherwise.
- Preserve known slot values unless the caller updates them.
- Do not invent facts.
- Apply any business rules stated in the live agent instructions for this client."
        )
    }
}

fn builtin_pack(default_voice: &str) -> ClientPack {
    let Value::Object(raw) = json!({
        "id": "default",
        "brand": {"company": "Val", "greeting": DEFAULT_GREETING, "tts_voice": default_voice},
        "ai": {"sop_instructions": FALLBACK_SOP_INSTRUCTIONS, "prompt_profile": "generic_receptionist"},
        "roleplay": {"default_scenario": "general", "scenarios": fallback_roleplay()}
    }) else {
        unreachable!()
    };
    ClientPack { raw }
}

/// Loads and caches client packs.
pub struct PackLoader {
    clients_dir: PathBuf,
    default_client_id: String,
    default_voice: String,
    cache: Mutex<HashMap<String, Arc<ClientPack>>>,
}

impl PackLoader {
    pub fn new(
        clients_dir: impl Into<PathBuf>,
        default_client_id: &str,
        default_voice: &str,
    ) -> Self {
        Self {
            clients_dir: clients_dir.into(),
            default_client_id: default_client_id.to_string(),
            default_voice: default_voice.to_string(),
            cache: Mutex::default(),
        }
    }

    fn read(&self, id: &str) -> Result<ClientPack, String> {
        let file = self.clients_dir.join(id).join("client.json");
        let raw = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
        let Value::Object(mut pack) = serde_json::from_str(&raw).map_err(|e| e.to_string())? else {
            return Err("client.json is not an object".into());
        };
        if js_str(pack.get("id")).is_empty() {
            pack.insert("id".into(), json!(id));
        }
        for section in ["brand", "ai", "roleplay"] {
            if !pack.get(section).map(Value::is_object).unwrap_or(false) {
                pack.insert(section.into(), json!({}));
            }
        }
        let company = js_str(pack["brand"].get("company"));
        let greeting = js_str(pack["brand"].get("greeting"));
        let sop = js_str(pack["ai"].get("sop_instructions"));
        if !sop.is_empty() {
            pack["ai"]["sop_instructions"] = json!(expand_template(&sop, &company, &greeting));
        }
        Ok(ClientPack { raw: pack })
    }

    /// Load a pack by id. Unknown or broken packs fall back to `default`, and a
    /// missing `default` falls back to the built-in generic receptionist.
    pub fn load(&self, client_id: &str) -> Arc<ClientPack> {
        let mut id = client_id.trim();
        if id.is_empty() {
            id = self.default_client_id.trim();
        }
        if id.is_empty() {
            id = "default";
        }
        if let Some(pack) = self.cache.lock().unwrap().get(id) {
            return pack.clone();
        }
        let pack = match self.read(id) {
            Ok(pack) => {
                tracing::info!(phase = "client_pack_loaded", client_id = id, company = %pack.company());
                Arc::new(pack)
            }
            Err(error) if id != "default" => {
                tracing::warn!(phase = "client_pack_fallback", client_id = id, %error);
                return self.load("default");
            }
            Err(_) => Arc::new(builtin_pack(&self.default_voice)),
        };
        self.cache
            .lock()
            .unwrap()
            .insert(id.to_string(), pack.clone());
        pack
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loader_with(packs: &[(&str, Value)]) -> (tempfile::TempDir, PackLoader) {
        let dir = tempfile::tempdir().unwrap();
        for (id, body) in packs {
            let folder = dir.path().join(id);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("client.json"), body.to_string()).unwrap();
        }
        let loader = PackLoader::new(dir.path(), "default", "eve");
        (dir, loader)
    }

    #[test]
    fn template_variables_expand() {
        assert_eq!(
            expand_template("Hi from {{company}}. {{greeting}}", "Acme", "G'day"),
            "Hi from Acme. G'day"
        );
        assert_eq!(expand_template("{{company}}", "", ""), "our company");
    }

    #[test]
    fn pack_loads_and_expands_sop() {
        let (_dir, loader) = loader_with(&[(
            "acme",
            json!({"brand": {"company": "Acme", "greeting": "Hello"}, "ai": {"sop_instructions": "You work at {{company}}."}}),
        )]);
        let pack = loader.load("acme");
        assert_eq!(pack.id(), "acme");
        assert_eq!(pack.sop_instructions(), "You work at Acme.");
        assert_eq!(pack.transfer_extension(), "1000");
    }

    #[test]
    fn unknown_pack_falls_back_to_default_then_builtin() {
        let (_dir, loader) = loader_with(&[("default", json!({"brand": {"company": "Generic"}}))]);
        assert_eq!(loader.load("missing").company(), "Generic");
        let (_dir2, empty) = loader_with(&[]);
        let builtin = empty.load("missing");
        assert_eq!(builtin.company(), "Val");
        assert_eq!(builtin.sop_instructions(), FALLBACK_SOP_INSTRUCTIONS);
    }

    #[test]
    fn roleplay_resolution() {
        let (_dir, loader) = loader_with(&[(
            "sbr",
            json!({"roleplay": {"default_scenario": "pay", "scenarios": {
                "pay": {"label": "Pays", "opening": "Hello?", "instructions": "x", "track": "collections"},
                "fees": {"label": "Fees", "opening": "What do you charge?", "instructions": "y"}
            }}}),
        )]);
        let pack = loader.load("sbr");
        assert_eq!(pack.roleplay("").opening, "Hello?");
        assert_eq!(pack.roleplay("FEES!").label, "Fees");
        assert_eq!(
            pack.roleplay("nope").label,
            "Pays",
            "unknown scenario uses the default"
        );
        let (_d, empty) = loader_with(&[]);
        assert_eq!(empty.load("").roleplay("zzz").label, "General caller");
    }

    #[test]
    fn intent_prompt_names_the_company() {
        let (_dir, loader) = loader_with(&[("default", json!({"brand": {"company": "Acme"}}))]);
        let prompt = loader.load("default").intent_prompt();
        assert!(
            prompt.starts_with("You label intent and extract slots for a live phone call at Acme.")
        );
        assert!(prompt.contains("action: continue|transfer|callback|decline_debt_type|complete"));
    }

    #[test]
    fn real_packs_in_repo_load() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/sip-stack/clients");
        if !dir.exists() {
            return;
        }
        let loader = PackLoader::new(&dir, "default", "eve");
        for id in ["default", "sbr", "demo"] {
            let pack = loader.load(id);
            assert!(!pack.sop_instructions().is_empty(), "{id} has instructions");
            assert!(!pack.sop_instructions().contains("{{company}}"));
            assert!(
                !pack.roleplay("").opening.is_empty(),
                "{id} has a default roleplay opening"
            );
        }
    }
}

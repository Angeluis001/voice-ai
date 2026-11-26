//! Enquiry slots and the merge rules used throughout a call.
use crate::{util::truthy, Doc};
use serde_json::{json, Value};

pub const SLOT_KEYS: [&str; 6] = [
    "debt_type",
    "amount",
    "staff_name",
    "reference",
    "escalation_offered",
    "action",
];

pub const ALLOWED_ACTIONS: [&str; 5] = [
    "continue",
    "transfer",
    "callback",
    "decline_debt_type",
    "complete",
];

pub fn empty_slots() -> Doc {
    let Value::Object(map) = json!({
        "debt_type": null,
        "amount": null,
        "staff_name": null,
        "reference": null,
        "escalation_offered": false,
        "action": "continue"
    }) else {
        unreachable!()
    };
    map
}

/// Lower-cases and validates an action; anything unknown becomes `continue`.
pub fn normalize_action(action: Option<&Value>) -> String {
    let raw = match action {
        Some(v) if truthy(v) => match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        },
        _ => "continue".to_string(),
    };
    let value = raw.trim().to_lowercase();
    if ALLOWED_ACTIONS.contains(&value.as_str()) {
        value
    } else {
        "continue".to_string()
    }
}

/// Merge `incoming` slot values over `existing`.
///
/// - Only the six known keys are read from `incoming`.
/// - `null` / empty string never overwrites a known value.
/// - `action` is normalised, `escalation_offered` is coerced to a boolean.
pub fn merge_slots(existing: Option<&Value>, incoming: Option<&Value>) -> Doc {
    let mut base = empty_slots();
    if let Some(Value::Object(existing)) = existing {
        for (k, v) in existing {
            base.insert(k.clone(), v.clone());
        }
    }
    let Some(Value::Object(incoming)) = incoming else {
        return base;
    };
    for key in SLOT_KEYS {
        let Some(value) = incoming.get(key) else {
            continue;
        };
        match key {
            "action" => {
                base.insert(key.into(), Value::String(normalize_action(Some(value))));
            }
            "escalation_offered" => {
                base.insert(key.into(), Value::Bool(truthy(value)));
            }
            _ => {
                let empty = value.is_null() || value.as_str() == Some("");
                if empty {
                    if base.get(key).map(Value::is_null).unwrap_or(true) {
                        base.insert(key.into(), Value::Null);
                    }
                } else {
                    base.insert(key.into(), value.clone());
                }
            }
        }
    }
    base
}

/// Slots of a session document, normalised.
pub fn doc_slots(doc: &Doc) -> Doc {
    merge_slots(None, doc.get("slots"))
}

/// The `action` slot as a string (`continue` when missing).
pub fn slot_action(slots: &Doc) -> String {
    normalize_action(slots.get("action"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_action_becomes_continue() {
        assert_eq!(normalize_action(Some(&json!(" Transfer "))), "transfer");
        assert_eq!(normalize_action(Some(&json!("hang_up"))), "continue");
        assert_eq!(normalize_action(None), "continue");
    }

    #[test]
    fn null_and_empty_never_overwrite() {
        let existing = json!({"debt_type": "invoice", "amount": "5000"});
        let incoming = json!({"debt_type": null, "amount": "", "staff_name": "Kim"});
        let merged = merge_slots(Some(&existing), Some(&incoming));
        assert_eq!(merged["debt_type"], "invoice");
        assert_eq!(merged["amount"], "5000");
        assert_eq!(merged["staff_name"], "Kim");
        assert_eq!(merged["action"], "continue");
        assert_eq!(merged["escalation_offered"], false);
    }

    #[test]
    fn action_and_escalation_are_coerced() {
        let incoming = json!({"action": "CALLBACK", "escalation_offered": 1, "junk": 1});
        let merged = merge_slots(None, Some(&incoming));
        assert_eq!(merged["action"], "callback");
        assert_eq!(merged["escalation_offered"], true);
        assert!(!merged.contains_key("junk"));
    }

    #[test]
    fn key_order_is_stable() {
        let merged = merge_slots(None, Some(&json!({"amount": "1"})));
        let keys: Vec<_> = merged.keys().map(String::as_str).collect();
        assert_eq!(keys, SLOT_KEYS);
    }
}

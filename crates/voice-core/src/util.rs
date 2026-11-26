//! Small helpers that mirror JavaScript value semantics used by the original bridge.
use serde_json::Value;

/// UTC timestamp in the same shape as JavaScript's `Date.toISOString()`.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// JavaScript truthiness.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `String(v || '')`: falsy values become an empty string.
pub fn js_str(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(v) => match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        },
        _ => String::new(),
    }
}

/// String field of an object, empty when missing or falsy.
pub fn field(doc: &crate::Doc, key: &str) -> String {
    js_str(doc.get(key))
}

/// First non-empty string of the candidates.
pub fn first_non_empty<const N: usize>(candidates: [String; N]) -> String {
    candidates
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}

/// `Number(v || 0)` for turn numbers.
pub fn js_num(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        Some(Value::Bool(true)) => 1.0,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn iso_has_millis_and_z() {
        let s = now_iso();
        assert!(s.ends_with('Z') && s.len() == 24, "{s}");
    }

    #[test]
    fn truthiness_matches_js() {
        assert!(!truthy(&json!(null)) && !truthy(&json!("")) && !truthy(&json!(0)));
        assert!(truthy(&json!("a")) && truthy(&json!(1)) && truthy(&json!({})));
        assert_eq!(js_str(Some(&json!(12))), "12");
        assert_eq!(js_str(Some(&json!(false))), "");
    }
}

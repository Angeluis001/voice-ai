//! Helpers that mirror the Python value semantics the original console relied on
//! (`x or y`, `str(x)`, `int(x)`, strict string fields).
use serde_json::{Map, Value};

static NULL: Value = Value::Null;

/// Raised when a file has a shape the Python console would have rejected; the
/// caller skips that file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Skip;

pub(crate) type Loose<T> = Result<T, Skip>;

/// Python truthiness of a JSON value.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `map.get(key)`, with `null` for a missing key.
pub(crate) fn get<'a>(map: &'a Map<String, Value>, key: &str) -> &'a Value {
    map.get(key).unwrap_or(&NULL)
}

/// `a or b or c`: the first truthy value, otherwise the last one.
pub(crate) fn or_chain<'a>(values: &[&'a Value]) -> &'a Value {
    for v in values {
        if truthy(v) {
            return v;
        }
    }
    values.last().copied().unwrap_or(&NULL)
}

/// Python `str(value)`.
pub(crate) fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => n.to_string(),
        other => py_json_dumps(other),
    }
}

/// `str(value or default)`.
pub(crate) fn str_or(v: &Value, default: &str) -> String {
    if truthy(v) {
        py_str(v)
    } else {
        default.to_string()
    }
}

/// A field that must be a string (a pydantic `str`).
pub(crate) fn req_str(v: &Value) -> Loose<String> {
    v.as_str().map(str::to_string).ok_or(Skip)
}

/// A field that must be a string or null (a pydantic `str | None`).
pub(crate) fn opt_str(v: &Value) -> Loose<Option<String>> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        _ => Err(Skip),
    }
}

/// `map.get(key, default)` for a field that must be a string.
pub(crate) fn str_default(map: &Map<String, Value>, key: &str, default: &str) -> Loose<String> {
    match map.get(key) {
        None => Ok(default.to_string()),
        Some(v) => req_str(v),
    }
}

/// The object stored under `key`, when it is one.
pub(crate) fn dict<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a Map<String, Value>> {
    map.get(key).and_then(Value::as_object)
}

/// Python `int(value)` that gives `None` instead of raising.
pub(crate) fn safe_int(v: &Value) -> Option<i64> {
    match v {
        Value::Null => None,
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => n.as_i64().or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite() && f.abs() < 9.0e18)
                .map(|f| f.trunc() as i64)
        }),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `float(value or 0.0)`.
pub(crate) fn py_float(v: &Value) -> Loose<f64> {
    if !truthy(v) {
        return Ok(0.0);
    }
    match v {
        Value::Bool(_) => Ok(1.0),
        Value::Number(n) => n.as_f64().ok_or(Skip),
        Value::String(s) => s.trim().parse().map_err(|_| Skip),
        _ => Err(Skip),
    }
}

/// `json.dumps(value, ensure_ascii=False)`: one line, `", "` and `": "` separators.
pub(crate) fn py_json_dumps(v: &Value) -> String {
    let mut out = String::new();
    write_py_json(v, &mut out);
    out
}

fn write_py_json(v: &Value, out: &mut String) {
    match v {
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_py_json(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push_str(": ");
                write_py_json(item, out);
            }
            out.push('}');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// Python `str.title()`: first letter of each word upper-cased, the rest lower-cased.
pub(crate) fn py_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_word = false;
    for c in s.chars() {
        if c.is_alphabetic() {
            if in_word {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            in_word = true;
        } else {
            out.push(c);
            in_word = false;
        }
    }
    out
}

/// Last path component, accepting both `/` and `\` separators.
pub(crate) fn basename(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn python_truthiness_and_str() {
        assert!(
            !truthy(&json!([])) && !truthy(&json!({})) && !truthy(&json!(0)) && !truthy(&json!(""))
        );
        assert!(truthy(&json!("0")) && truthy(&json!([0])));
        assert_eq!(py_str(&json!(1500)), "1500");
        assert_eq!(py_str(&json!(true)), "True");
        assert_eq!(str_or(&json!(null), "x"), "x");
        assert_eq!(
            or_chain(&[&json!(""), &json!(null), &json!("")]),
            &json!("")
        );
    }

    #[test]
    fn safe_int_matches_python_int() {
        assert_eq!(safe_int(&json!("3")), Some(3));
        assert_eq!(safe_int(&json!(" 4 ")), Some(4));
        assert_eq!(safe_int(&json!(2.9)), Some(2));
        assert_eq!(safe_int(&json!("2.9")), None);
        assert_eq!(safe_int(&json!("")), None);
        assert_eq!(safe_int(&json!(null)), None);
        assert_eq!(safe_int(&json!([1])), None);
    }

    #[test]
    fn dumps_title_and_basename() {
        assert_eq!(
            py_json_dumps(&json!({"a": 1, "b": ["x", null]})),
            r#"{"a": 1, "b": ["x", null]}"#
        );
        assert_eq!(py_json_dumps(&json!({})), "{}");
        assert_eq!(py_title("angry customer_2x"), "Angry Customer_2X");
        assert_eq!(basename("/var/lib/media/abc-call.wav"), "abc-call.wav");
        assert_eq!(basename(r"C:\media\abc-call.wav"), "abc-call.wav");
        assert_eq!(basename("abc.wav"), "abc.wav");
    }
}

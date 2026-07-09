//! The single JSON entry point for the wire layer (architecture D3).
//! miniserde's own errors carry no detail, so every parse failure here wraps
//! the raw offending line — for short SSE chunks that is the diagnostic.

use miniserde::json::{Number, Value};

use crate::error::{Error, Result};

pub fn from_line<T: miniserde::Deserialize>(line: &str) -> Result<T> {
    miniserde::json::from_str(line)
        .map_err(|_| Error::Provider(format!("unparseable chunk: {line}")))
}

pub fn to_string<T: miniserde::Serialize>(value: &T) -> String {
    miniserde::json::to_string(value)
}

// Small builders keeping the adapters' request-body assembly readable.

pub fn str_value(v: &str) -> Value {
    Value::String(v.to_owned())
}

pub fn u64_value(v: u64) -> Value {
    Value::Number(Number::U64(v))
}

pub fn f64_value(v: f64) -> Value {
    Value::Number(Number::F64(v))
}

/// Typed extractors for asserting on `Value` trees in adapter tests —
/// miniserde's `Value` has no `PartialEq`, so equality runs through these.
#[cfg(test)]
pub(crate) mod testutil {
    use miniserde::json::{Number, Object, Value};

    pub(crate) fn str_of(object: &Object, key: &str) -> Option<String> {
        match object.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        }
    }

    pub(crate) fn u64_of(object: &Object, key: &str) -> Option<u64> {
        match object.get(key) {
            Some(Value::Number(Number::U64(n))) => Some(*n),
            _ => None,
        }
    }

    pub(crate) fn f64_of(object: &Object, key: &str) -> Option<f64> {
        match object.get(key) {
            Some(Value::Number(Number::F64(n))) => Some(*n),
            _ => None,
        }
    }

    pub(crate) fn bool_of(object: &Object, key: &str) -> Option<bool> {
        match object.get(key) {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::from_line;
    use miniserde::Deserialize;

    #[derive(Deserialize, Debug)]
    struct Probe {
        value: u64,
    }

    #[test]
    fn parses_into_the_requested_shape() {
        let probe: Probe = from_line(r#"{"value":7,"ignored":"x"}"#).unwrap();
        assert_eq!(probe.value, 7);
    }

    #[test]
    fn failure_carries_the_raw_line() {
        let err = from_line::<Probe>("{broken").unwrap_err();
        assert_eq!(err.to_string(), "unparseable chunk: {broken");
    }
}

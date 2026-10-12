use serde::Serialize;
use serde_json::Value;

use crate::error::{LensError, Result};

/// Serialize without ever allocating more than `limit` output bytes. This
/// avoids materializing attacker-sized strings before checking their size.
pub fn to_bounded_vec<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>> {
    struct CappedWriter {
        bytes: Vec<u8>,
        limit: usize,
        exceeded: bool,
    }
    impl std::io::Write for CappedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                self.exceeded = true;
                return Err(std::io::Error::other("JSON byte limit exceeded"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = CappedWriter {
        bytes: Vec::with_capacity(limit),
        limit,
        exceeded: false,
    };
    let result = serde_json::to_writer(&mut writer, value);
    if writer.exceeded {
        return Err(LensError::LimitExceeded {
            message: "JSON byte limit exceeded".into(),
        });
    }
    result.map_err(LensError::Json)?;
    Ok(writer.bytes)
}

/// Recursively sort JSON object keys to guarantee deterministic byte serialization.
fn sort_json_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted: std::collections::BTreeMap<String, Value> =
                std::collections::BTreeMap::new();
            for (k, v) in map {
                sorted.insert(k, sort_json_keys(v));
            }
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(list) => Value::Array(list.into_iter().map(sort_json_keys).collect()),
        scalar => scalar,
    }
}

/// Serializes any Serializable data structure to a deterministic, sorted-key pretty JSON string.
pub fn to_deterministic_pretty<T: Serialize>(value: &T) -> Result<String> {
    let uncanonical = serde_json::to_value(value).map_err(LensError::Json)?;
    let canonical = sort_json_keys(uncanonical);
    serde_json::to_string_pretty(&canonical).map_err(LensError::Json)
}

/// Serializes any Serializable data structure to a deterministic compact JSON string.
pub fn to_deterministic_string<T: Serialize>(value: &T) -> Result<String> {
    let uncanonical = serde_json::to_value(value).map_err(LensError::Json)?;
    let canonical = sort_json_keys(uncanonical);
    serde_json::to_string(&canonical).map_err(LensError::Json)
}

#[cfg(test)]
mod tests {
    #[test]
    fn capped_writer_stops_on_large_strings_and_counts_escaping() {
        assert!(matches!(
            to_bounded_vec(&"x".repeat(8 * 1024 * 1024), 4096),
            Err(LensError::LimitExceeded { .. })
        ));
        assert!(to_bounded_vec(&"\n".repeat(2048), 4096).is_err());
        let value = vec!["한글", "bounded"];
        assert_eq!(
            to_bounded_vec(&value, 4096).unwrap(),
            serde_json::to_vec(&value).unwrap()
        );
    }
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_deterministic_key_ordering() {
        let mut map = HashMap::new();
        map.insert("z", 1);
        map.insert("a", 2);
        map.insert("m", 3);

        let json = to_deterministic_string(&map).unwrap();
        assert_eq!(json, r#"{"a":2,"m":3,"z":1}"#);
    }
}

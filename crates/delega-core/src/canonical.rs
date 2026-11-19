//! One key order for every JSON document delega writes.
//!
//! A JSON object has no order, but the documents delega prints and serves are
//! read by programs and compared as text, so they must not depend on how the
//! build was configured. `serde_json` keeps keys sorted unless some crate in the
//! build turns on its `preserve_order` feature, and one does: the MCP server's
//! `tower-mcp` does it for every crate built with it, so `delega-core` built
//! alone and built in the workspace would otherwise print the same value with
//! different key orders. [`sorted`] puts every object's keys in alphabetical
//! order, which is what an unconfigured `serde_json` produces, and the places
//! that write a document built from a [`Value`] call it.
//!
//! A struct serialized directly is not affected: its fields come out in
//! declaration order in either build.

use serde_json::Value;

/// The value with every object's keys in alphabetical order, at any depth.
/// Arrays keep their order.
pub fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// The serialization of `value` as a [`Value`] with its keys sorted.
pub fn to_sorted_value(value: &impl serde::Serialize) -> Result<Value, serde_json::Error> {
    serde_json::to_value(value).map(sorted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_are_sorted_at_every_depth_and_arrays_keep_their_order() {
        let mut inner = serde_json::Map::new();
        inner.insert("z".into(), json!(1));
        inner.insert("a".into(), json!([{"y": 1, "b": 2}, 3]));
        let mut outer = serde_json::Map::new();
        outer.insert("m".into(), Value::Object(inner));
        outer.insert("b".into(), json!(["z", "a"]));
        let text = sorted(Value::Object(outer)).to_string();
        assert_eq!(text, r#"{"b":["z","a"],"m":{"a":[{"b":2,"y":1},3],"z":1}}"#);
    }
}

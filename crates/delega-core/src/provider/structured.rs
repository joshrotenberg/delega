//! Reading a structured answer out of the final text.
//!
//! The assignment asks for one JSON value that satisfies `answer_schema`. The
//! final text is parsed as JSON, allowing for a single code fence around it,
//! and validated against the schema. Neither provider's own schema option is
//! used.

use serde_json::Value;

/// Parse the final answer as one JSON value.
pub(super) fn extract(text: &str) -> Result<Value, String> {
    let trimmed = text.trim();
    let body = unfence(trimmed).unwrap_or(trimmed);
    serde_json::from_str(body).map_err(|e| format!("the final answer is not JSON: {e}"))
}

/// The inside of a single fenced block that spans the whole text.
fn unfence(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("```")?;
    let rest = rest.strip_suffix("```")?;
    let (_label, body) = rest.split_once('\n')?;
    Some(body.trim())
}

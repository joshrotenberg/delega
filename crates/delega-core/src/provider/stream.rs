//! What a provider's output stream is reduced to.
//!
//! Both providers write one JSON event per line. [`for_each_event`] frames the
//! lines and the provider modules turn the events into a [`Parsed`].

use serde_json::{Map, Value};

use crate::contract::result::{DeniedRequest, Usage};

use super::limits::{DENIAL_TARGET_CHARS, MAX_DENIED, MAX_LINE_BYTES};
use super::refusal::clip;

/// How the provider's last terminal event ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) enum Terminal {
    /// No terminal event was seen.
    #[default]
    None,
    /// The provider reported success.
    Success,
    /// The provider reported failure, with its message.
    Failure(String),
}

/// How many lines were read and how many could not be used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct LineStats {
    /// Lines that were not JSON objects.
    pub unparseable: usize,
    /// Lines longer than the limit, skipped.
    pub oversize: usize,
}

/// Everything taken from one provider stream.
#[derive(Debug, Clone, Default)]
pub(super) struct Parsed {
    /// The provider's own session id.
    pub session_id: Option<String>,
    /// The final answer as the provider's terminal event reported it.
    pub final_text: Option<String>,
    /// The last assistant message seen, a fallback when the terminal event
    /// carries no text.
    pub last_text: Option<String>,
    /// How the stream ended.
    pub terminal: Terminal,
    /// Token counts and cost.
    pub usage: Option<Usage>,
    /// Requests answered no.
    pub denied: Vec<DeniedRequest>,
    /// How many denials were not recorded because of the bound.
    pub denied_omitted: usize,
    /// Messages from provider error events that did not end the run.
    pub errors: Vec<String>,
    /// Line framing counts.
    pub stats: LineStats,
}

impl Parsed {
    /// Record a denied request, bounded.
    pub fn deny(&mut self, tool: &str, target: Option<&str>) {
        if self.denied.len() >= MAX_DENIED {
            self.denied_omitted += 1;
            return;
        }
        let request = match target {
            Some(t) if !t.is_empty() => {
                format!("{tool}: {}", clip(t, DENIAL_TARGET_CHARS))
            }
            _ => tool.to_string(),
        };
        self.denied.push(DeniedRequest { request });
    }

    /// The best text for a result that did not complete.
    pub fn partial_text(&self) -> String {
        self.final_text
            .clone()
            .or_else(|| self.last_text.clone())
            .unwrap_or_default()
    }
}

/// Call `handle` for each line of `bytes` that is a JSON object. Other lines
/// are counted in the returned stats.
pub(super) fn for_each_event(
    bytes: &[u8],
    mut handle: impl FnMut(&Map<String, Value>),
) -> LineStats {
    let mut stats = LineStats::default();
    for line in bytes.split(|b| *b == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            stats.oversize += 1;
            continue;
        }
        match serde_json::from_slice::<Value>(line) {
            Ok(Value::Object(event)) => handle(&event),
            _ => stats.unparseable += 1,
        }
    }
    stats
}

/// A string field, when present and non-empty.
pub(super) fn str_field<'a>(event: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    event
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// A count field.
pub(super) fn count(object: &Value, key: &str) -> Option<u64> {
    object.get(key).and_then(Value::as_u64)
}

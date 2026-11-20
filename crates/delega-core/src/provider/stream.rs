//! What a provider's output stream is reduced to.
//!
//! Both providers write one JSON event per line. [`for_each_event`] frames the
//! lines and the provider modules turn the events into a [`Parsed`].

use serde_json::{Map, Value};

use crate::contract::result::{DeniedRequest, Usage};

use super::limits::{
    DENIAL_REASON_CHARS, DENIAL_TARGET_CHARS, MAX_DENIED, MAX_LINE_BYTES, MAX_SESSION_IDS,
};
use super::refusal::clip;

/// How the provider's last terminal event ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) enum Terminal {
    /// No terminal event was seen.
    #[default]
    None,
    /// The provider reported success.
    Success,
    /// The provider reported failure: its message and, for a failure the
    /// provider names, a stable error kind (a turn or budget limit).
    Failure {
        message: String,
        code: Option<&'static str>,
    },
}

/// How many lines were read and how many could not be used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct LineStats {
    /// Lines that were not JSON objects.
    pub unparseable: usize,
    /// Lines longer than the limit, skipped.
    pub oversize: usize,
    /// Lines that held invalid UTF-8, decoded with replacement characters.
    pub replaced: usize,
}

/// Everything taken from one provider stream.
#[derive(Debug, Clone, Default)]
pub(super) struct Parsed {
    /// The provider's own session id: the latest one the stream reported.
    pub session_id: Option<String>,
    /// Every distinct session id the stream reported, in order, bounded by
    /// [`MAX_SESSION_IDS`].
    pub session_ids: Vec<String>,
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
    /// Whether the run stopped because of the latest denial: the provider
    /// reported a failure that is not a named limit, a denial was recorded and
    /// nothing after it went on or went wrong.
    pub stopped_by_denial: bool,
    /// Notes for the result, such as a second terminal event.
    pub notes: Vec<String>,
    /// Line framing counts.
    pub stats: LineStats,
}

impl Parsed {
    /// Record a session id the stream reported.
    pub fn saw_session(&mut self, id: &str) {
        self.session_id = Some(id.to_string());
        if self.session_ids.len() < MAX_SESSION_IDS && !self.session_ids.iter().any(|s| s == id) {
            self.session_ids.push(id.to_string());
        }
    }

    /// Record a denied request, bounded.
    pub fn deny(&mut self, tool: &str, target: Option<&str>, reason: Option<&str>) {
        if self.denied.len() >= MAX_DENIED {
            self.denied_omitted += 1;
            return;
        }
        let mut request = match target {
            Some(t) if !t.is_empty() => {
                format!("{tool}: {}", clip(t, DENIAL_TARGET_CHARS))
            }
            _ => tool.to_string(),
        };
        if let Some(reason) = reason.filter(|r| !r.is_empty()) {
            request.push_str(&format!(" ({})", clip(reason, DENIAL_REASON_CHARS)));
        }
        self.denied.push(DeniedRequest { request });
    }

    /// Record that another terminal event came after the first.
    pub fn note_second_terminal(&mut self) {
        let note = "a second terminal event was seen after the first and was ignored";
        if !self.notes.iter().any(|n| n == note) {
            self.notes.push(note.to_string());
        }
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
        // Invalid UTF-8 is decoded with replacement characters, not rejected.
        let text = String::from_utf8_lossy(line);
        if matches!(text, std::borrow::Cow::Owned(_)) {
            stats.replaced += 1;
        }
        match serde_json::from_str::<Value>(&text) {
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

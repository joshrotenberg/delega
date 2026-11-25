//! What a provider's output stream is reduced to.
//!
//! Both providers write one JSON event per line. [`for_each_event`] frames the
//! lines and the provider modules turn the events into a [`Parsed`].

use serde_json::{Map, Value};

use crate::contract::result::{DeniedRequest, Usage};

use super::limits::{
    DENIAL_TARGET_CHARS, DENIAL_TOOL_CHARS, MAX_DENIED, MAX_LINE_BYTES, MAX_SESSION_IDS,
};
use super::progress::bounded;
use super::redact::Redactor;
use super::refusal::clip;

/// A denied request as the provider reported it. The raw tool name decides how
/// the denial is classified; what a result lists is [`Denial::shown`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Denial {
    /// The tool, as the provider named it.
    pub tool: String,
    /// The command or path, when there is one, cut to [`DENIAL_TARGET_CHARS`].
    pub target: Option<String>,
    /// Why the provider said it was denied, when it did, already redacted
    /// and cut to [`DENIAL_REASON_CHARS`](super::limits::DENIAL_REASON_CHARS)
    /// by the parser.
    pub reason: Option<String>,
}

impl Denial {
    /// The request as a result, progress and activity show it. The tool name
    /// is shown as [`bounded`] makes it, cut to [`DENIAL_TOOL_CHARS`]; the
    /// reason is shown as stored. The command or path is shown as the
    /// provider asked for it, cut only: it is what the caller needs to see to
    /// decide what to grant.
    pub fn shown(&self, redactor: &Redactor) -> DeniedRequest {
        let tool = bounded(&self.tool, redactor, DENIAL_TOOL_CHARS);
        let mut request = match self.target.as_deref() {
            Some(t) if !t.is_empty() => format!("{tool}: {t}"),
            _ => tool,
        };
        if let Some(reason) = self.reason.as_deref().filter(|r| !r.is_empty()) {
            request.push_str(&format!(" ({reason})"));
        }
        DeniedRequest { request }
    }
}

/// How the provider's last terminal event ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) enum Terminal {
    /// No terminal event was seen.
    #[default]
    None,
    /// The provider reported success.
    Success,
    /// The provider reported failure: its whole message (`settle` redacts it
    /// and then clips it) and, for a failure the
    /// provider names or delega recognises, a stable error kind (a turn,
    /// budget or usage limit, an account, capacity or model failure).
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
    /// Every distinct session id the whole stream reported, in order, bounded
    /// by [`MAX_SESSION_IDS`]. Events after the first terminal event count.
    pub session_ids: Vec<String>,
    /// Hashes of the distinct session ids past the bound, to count them.
    pub session_ids_omitted: std::collections::HashSet<u64>,
    /// The first session id the stream reported that `session.resume` would
    /// refuse. It is in none of the fields above; `settle` notes it, redacted
    /// and cut. Its length is bounded by the line limit.
    pub refused_session: Option<String>,
    /// The final answer as the provider's terminal event reported it.
    pub final_text: Option<String>,
    /// The last assistant message seen, a fallback when the terminal event
    /// carries no text.
    pub last_text: Option<String>,
    /// How the stream ended.
    pub terminal: Terminal,
    /// The run's own token counts and cost that the stream gives whether or
    /// not the run resumed a session.
    pub usage: Option<Usage>,
    /// The run's own token counts and cost when the run did not resume a
    /// session, so the session is the run and its totals are the run's.
    pub fresh_usage: Option<Usage>,
    /// The provider's totals for the whole session through this run.
    pub session_usage: Option<Usage>,
    /// Requests answered no, as reported. The texts are bounded only by the
    /// captured output they came from until [`Denial::shown`] cuts them.
    pub denied: Vec<Denial>,
    /// How many denials were not recorded because of the bound.
    pub denied_omitted: usize,
    /// How many provider error events (and Claude Code API retries) were
    /// seen before the first terminal event.
    pub error_count: usize,
    /// The whole message of the last of them. `settle` redacts it and then
    /// clips it; only the last is kept, so the state stays bounded.
    pub last_error: Option<String>,
    /// The error kind the last error event names, used when no terminal event
    /// follows. `None` is `provider_error`.
    pub error_code: Option<&'static str>,
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
    /// Record a session id the stream reported before its first terminal
    /// event: it becomes the run's session id.
    pub fn saw_session(&mut self, id: &str) {
        if self.refuse_session(id) {
            return;
        }
        self.session_id = Some(id.to_string());
        self.saw_late_session(id);
    }

    /// Whether `id` is one `session.resume` would refuse, so it cannot be
    /// resumed and is not kept. The first such id is remembered for a note.
    fn refuse_session(&mut self, id: &str) -> bool {
        if crate::contract::validate::session_id_problems(id).is_empty() {
            return false;
        }
        self.refused_session.get_or_insert_with(|| id.to_string());
        true
    }

    /// Record a session id the stream reported after its first terminal
    /// event. It counts towards the ids the stream reported but does not
    /// change the run's session id, which the first terminal event decided.
    pub fn saw_late_session(&mut self, id: &str) {
        use std::hash::{Hash, Hasher};
        if self.refuse_session(id) || self.session_ids.iter().any(|s| s == id) {
            return;
        }
        if self.session_ids.len() < MAX_SESSION_IDS {
            self.session_ids.push(id.to_string());
        } else {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            id.hash(&mut hasher);
            self.session_ids_omitted.insert(hasher.finish());
        }
    }

    /// Record a provider error event's message.
    pub fn saw_error(&mut self, message: &str) {
        self.error_count += 1;
        self.last_error = Some(message.to_string());
    }

    /// Record a denied request, up to [`MAX_DENIED`] of them.
    pub fn deny(&mut self, tool: &str, target: Option<&str>, reason: Option<&str>) {
        if self.denied.len() >= MAX_DENIED {
            self.denied_omitted += 1;
            return;
        }
        self.denied.push(Denial {
            tool: tool.to_string(),
            target: target.map(|t| clip(t, DENIAL_TARGET_CHARS)),
            reason: reason.map(str::to_string),
        });
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

/// `usage` unless every field is empty.
pub(super) fn non_empty(usage: Usage) -> Option<Usage> {
    let empty = usage.input_tokens.is_none()
        && usage.output_tokens.is_none()
        && usage.cached_tokens.is_none()
        && usage.reasoning_tokens.is_none()
        && usage.cost_usd.is_none();
    (!empty).then_some(usage)
}

/// A cost: a finite number that is not negative.
pub(super) fn cost(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|c| c.is_finite() && *c >= 0.0)
}

/// A count field.
pub(super) fn count(object: &Value, key: &str) -> Option<u64> {
    object.get(key).and_then(Value::as_u64)
}

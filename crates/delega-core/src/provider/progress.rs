//! Live progress of a run, as neutral events.
//!
//! A caller that wants to follow a run sets [`ProviderOptions::progress`]
//! (super::ProviderOptions::progress) to a [`ProgressSink`]. The provider layer
//! calls it, from the task that follows the provider, with:
//!
//! 1. [`ProgressEvent::Started`] when the provider process is about to start,
//! 2. [`ProgressEvent::Provider`] for each line of the provider's output as it
//!    arrives, reduced to a kind and a short summary,
//! 3. [`ProgressEvent::Denied`] for each request the result lists as denied,
//!    after the provider has finished (Claude Code reports its denials in its
//!    final event, so a denial cannot be announced earlier for both providers),
//! 4. [`ProgressEvent::Finished`], always last, also for a run that never
//!    started.
//!
//! What a summary holds, exactly:
//!
//! - an agent message: its length in characters, never its text,
//! - a tool call or result (Claude Code): the tool name or ok/error,
//! - an item (Codex): its type and status, never its command,
//! - an error event: at most [`PROGRESS_SUMMARY_CHARS`] characters of its
//!   text, with credentials removed. Provider error text is free text, so it
//!   can name anything the provider chose to say.
//!
//! A kind is the provider's event type with credentials removed, cut to
//! [`PROGRESS_KIND_CHARS`] characters. Real types are short words, so they
//! arrive unchanged.
//!
//! A [`ProgressEvent::Denied`] carries the request exactly as the result lists
//! it in `permissions.denied`: the tool name and reason with credentials
//! removed, the denied command or path as asked for, each part cut.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Map, Value};

use crate::contract::outcome::Outcome;
use crate::contract::request::Provider;
use crate::contract::result::RunResult;
use crate::process::OutputTap;

use super::limits::{
    MAX_LINE_BYTES, PROGRESS_KIND_CHARS, PROGRESS_SUMMARY_CHARS, PROGRESS_TEXT_CHARS,
};
use super::redact::Redactor;
use super::refusal::clip;
use super::stream::str_field;

/// One thing that happened during a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressEvent {
    /// The provider process is about to start.
    Started {
        /// The provider.
        provider: Provider,
        /// The executable that starts.
        program: String,
    },
    /// The provider wrote an event.
    Provider {
        /// The provider's own event type, such as `assistant` or
        /// `item.completed`, with credentials removed and cut to
        /// [`PROGRESS_KIND_CHARS`].
        kind: String,
        /// A short description, possibly empty.
        summary: String,
        /// Whether the provider's event type was exactly `system`, before
        /// redaction and cutting. `kind` alone cannot tell: a type such as
        /// `" system "` is shown as `system` too.
        system: bool,
    },
    /// The text of one agent message, with credentials removed and cut to
    /// [`PROGRESS_TEXT_CHARS`]. Sent, after the [`Provider`](Self::Provider)
    /// event that gives the message's length, only when
    /// [`ProviderOptions::progress_text`](super::ProviderOptions::progress_text)
    /// is set.
    Message {
        /// The message text.
        text: String,
    },
    /// A request the provider made was answered no.
    Denied {
        /// What the provider asked to do, as in `permissions.denied`.
        request: String,
    },
    /// Provider output was not reported as events because the sink was too
    /// slow. Sent once, after the provider has ended and before `Finished`,
    /// only when the count is not zero.
    Dropped {
        /// How many chunks of provider output were not delivered. A chunk
        /// holds part of a line, one line or several.
        chunks: u64,
    },
    /// The run ended. Always the last event.
    Finished {
        /// How it ended.
        outcome: Outcome,
    },
}

/// Receives progress events. It must return quickly and must not block: it
/// runs inside the task that reads the provider's output.
pub type ProgressSink = Arc<dyn Fn(ProgressEvent) + Send + Sync>;

/// The sink of one run, if any, and the events built from a run's facts.
#[derive(Clone, Default)]
pub(super) struct Progress {
    sink: Option<ProgressSink>,
    /// Whether message texts are sent too.
    text: bool,
    /// Chunks of output the process layer could not hand to the tap.
    dropped: Arc<AtomicU64>,
    /// True once the run's last events were sent. The tap runs on a thread
    /// that can be left behind by a slow sink, so without this it could
    /// deliver an event after `Finished`.
    closed: Arc<Mutex<bool>>,
}

impl Progress {
    pub fn new(sink: Option<ProgressSink>, text: bool) -> Self {
        Self {
            sink,
            text,
            dropped: Arc::default(),
            closed: Arc::default(),
        }
    }

    /// Send an event unless the run's last event has been sent.
    pub fn emit(&self, event: ProgressEvent) {
        let closed = self.closed.lock().unwrap_or_else(PoisonError::into_inner);
        if let (false, Some(sink)) = (*closed, &self.sink) {
            sink(event);
        }
    }

    /// The counter the process layer adds dropped chunks to.
    pub fn drop_counter(&self) -> Arc<AtomicU64> {
        self.dropped.clone()
    }

    /// Announce the denials, any dropped output and the end of a run, and say
    /// in the result's notes that output was dropped. Nothing is sent after
    /// this returns, so `Finished` is always the last event.
    pub fn finished(&self, result: &mut RunResult) {
        let Some(sink) = &self.sink else {
            return;
        };
        let mut closed = self.closed.lock().unwrap_or_else(PoisonError::into_inner);
        if *closed {
            return;
        }
        for denied in &result.permissions.denied {
            sink(ProgressEvent::Denied {
                request: denied.request.clone(),
            });
        }
        let chunks = self.dropped.load(Ordering::Relaxed);
        if chunks > 0 {
            result.provider_notes.push(format!(
                "{chunks} chunks of provider output were not reported as progress events because \
                 the progress consumer was too slow"
            ));
            sink(ProgressEvent::Dropped { chunks });
        }
        sink(ProgressEvent::Finished {
            outcome: result.outcome,
        });
        *closed = true;
    }

    /// A tap for the provider's standard output that turns each line into
    /// events, or `None` when nobody listens.
    pub fn tap(&self, provider: Provider, redactor: Redactor) -> Option<OutputTap> {
        self.sink.as_ref()?;
        let progress = self.clone();
        let framer = Mutex::new(Framer::default());
        Some(Arc::new(move |chunk: &[u8]| {
            let lines = framer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .feed(chunk);
            for line in lines {
                for (kind, summary, system) in describe_line(provider, &line, &redactor) {
                    progress.emit(ProgressEvent::Provider {
                        kind,
                        summary,
                        system,
                    });
                }
                if progress.text {
                    for text in message_texts(provider, &line, &redactor) {
                        progress.emit(ProgressEvent::Message { text });
                    }
                }
            }
        }))
    }
}

/// Splits a byte stream into lines. A line longer than [`MAX_LINE_BYTES`] is
/// dropped whole.
#[derive(Default)]
struct Framer {
    buffer: Vec<u8>,
    skipping: bool,
}

impl Framer {
    /// The lines completed by `chunk`, without their line ends.
    fn feed(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        let mut lines = Vec::new();
        for piece in chunk.split_inclusive(|b| *b == b'\n') {
            if !self.skipping {
                if self.buffer.len() + piece.len() > MAX_LINE_BYTES {
                    self.skipping = true;
                    self.buffer.clear();
                } else {
                    self.buffer.extend_from_slice(piece);
                }
            }
            if piece.ends_with(b"\n") {
                if !self.skipping {
                    lines.push(std::mem::take(&mut self.buffer));
                }
                self.buffer.clear();
                self.skipping = false;
            }
        }
        lines
    }
}

/// The events one output line stands for. A line that is not a JSON object
/// stands for none. Kinds are matched as the provider wrote them and only then
/// redacted and cut, like summaries. The third value says whether the type was
/// exactly `system` as written.
fn describe_line(
    provider: Provider,
    line: &[u8],
    redactor: &Redactor,
) -> Vec<(String, String, bool)> {
    let Ok(Value::Object(event)) = serde_json::from_slice::<Value>(line.trim_ascii()) else {
        return Vec::new();
    };
    let described = match provider {
        Provider::Claude => describe_claude(&event),
        Provider::Codex => describe_codex(&event),
    };
    described
        .into_iter()
        .map(|(kind, summary)| {
            (
                bounded(&kind, redactor, PROGRESS_KIND_CHARS),
                bounded(&summary, redactor, PROGRESS_SUMMARY_CHARS),
                kind == "system",
            )
        })
        .collect()
}

/// The texts of the agent messages one output line holds, with credentials
/// removed and cut to [`PROGRESS_TEXT_CHARS`]. Claude Code: each text block of
/// an assistant event. Codex: an agent message item that completed.
fn message_texts(provider: Provider, line: &[u8], redactor: &Redactor) -> Vec<String> {
    let Ok(Value::Object(event)) = serde_json::from_slice::<Value>(line.trim_ascii()) else {
        return Vec::new();
    };
    let texts: Vec<String> = match provider {
        Provider::Claude if str_field(&event, "type") == Some("assistant") => event
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str).map(str::to_string))
            .collect(),
        Provider::Codex if str_field(&event, "type") == Some("item.completed") => event
            .get("item")
            .filter(|i| i.get("type").and_then(Value::as_str) == Some("agent_message"))
            .and_then(|i| i.get("text").and_then(Value::as_str))
            .map(str::to_string)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    };
    texts
        .into_iter()
        .filter(|t| !t.is_empty())
        .map(|t| clip(&redactor.apply(&t), PROGRESS_TEXT_CHARS))
        .collect()
}

/// Provider text with credentials removed, then joined to one line, then cut
/// to `max` characters. Redaction comes first: a configured value is matched
/// exactly as passed, so joining its whitespace first could hide it.
pub(super) fn bounded(text: &str, redactor: &Redactor, max: usize) -> String {
    let redacted = redactor.apply(text);
    let one_line = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
    clip(&one_line, max)
}

/// How an agent message is shown: its length, never its text.
fn message_size(text: &str) -> String {
    format!("message: {} chars", text.chars().count())
}

fn kind_of(event: &Map<String, Value>) -> String {
    str_field(event, "type").unwrap_or("unknown").to_string()
}

fn describe_claude(event: &Map<String, Value>) -> Vec<(String, String)> {
    let kind = kind_of(event);
    let blocks = || {
        event
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
    };
    match kind.as_str() {
        "assistant" => {
            let described: Vec<(String, String)> = blocks()
                .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                        Some(("assistant".to_string(), message_size(text)))
                    }
                    Some("tool_use") => {
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                        Some(("assistant".to_string(), format!("tool_use: {name}")))
                    }
                    _ => None,
                })
                .collect();
            if described.is_empty() {
                vec![(kind, String::new())]
            } else {
                described
            }
        }
        "user" => {
            let described: Vec<(String, String)> = blocks()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                .map(|b| {
                    let failed = b.get("is_error").and_then(Value::as_bool) == Some(true);
                    let state = if failed { "error" } else { "ok" };
                    ("user".to_string(), format!("tool_result: {state}"))
                })
                .collect();
            if described.is_empty() {
                vec![(kind, String::new())]
            } else {
                described
            }
        }
        "system" | "result" => {
            let subtype = str_field(event, "subtype").unwrap_or("").to_string();
            vec![(kind, subtype)]
        }
        "error" => {
            let message = str_field(event, "message").unwrap_or("").to_string();
            vec![(kind, message)]
        }
        _ => vec![(kind, String::new())],
    }
}

fn describe_codex(event: &Map<String, Value>) -> Vec<(String, String)> {
    let kind = kind_of(event);
    match kind.as_str() {
        "item.started" | "item.completed" => {
            let item = event.get("item");
            let item_type = item
                .and_then(|i| i.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("item");
            let summary = if item_type == "agent_message" {
                let text = item
                    .and_then(|i| i.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                message_size(text)
            } else {
                let status = item
                    .and_then(|i| i.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                format!("{item_type} {status}")
            };
            vec![(kind, summary)]
        }
        "turn.failed" => {
            let message = event
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            vec![(kind, message)]
        }
        "error" => {
            let message = str_field(event, "message").unwrap_or("").to_string();
            vec![(kind, message)]
        }
        _ => vec![(kind, String::new())],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(framer: &mut Framer, chunk: &str) -> Vec<String> {
        framer
            .feed(chunk.as_bytes())
            .into_iter()
            .map(|l| String::from_utf8(l).expect("utf-8"))
            .collect()
    }

    #[test]
    fn a_line_split_across_chunks_is_reported_once_when_it_completes() {
        let mut framer = Framer::default();
        assert!(lines(&mut framer, "{\"a\":").is_empty());
        assert_eq!(lines(&mut framer, "1}\n{\"b\""), ["{\"a\":1}\n"]);
        assert_eq!(lines(&mut framer, ":2}\n"), ["{\"b\":2}\n"]);
    }

    #[test]
    fn several_lines_in_one_chunk_are_all_reported() {
        let mut framer = Framer::default();
        assert_eq!(lines(&mut framer, "a\nb\nc"), ["a\n", "b\n"]);
    }

    #[test]
    fn an_oversize_line_is_dropped_and_the_next_one_still_reads() {
        let mut framer = Framer::default();
        let big = vec![b'x'; MAX_LINE_BYTES + 1];
        assert!(framer.feed(&big).is_empty());
        let got = framer.feed(b"tail\nok\n");
        assert_eq!(got, [b"ok\n".to_vec()]);
    }

    #[test]
    fn an_agent_message_is_shown_by_length_only() {
        let redactor = Redactor::default();
        let event = serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": "see /etc/passwd and run rm -rf x"}]}
        });
        let out = describe_line(Provider::Claude, event.to_string().as_bytes(), &redactor);
        assert_eq!(
            out,
            [(
                "assistant".to_string(),
                "message: 32 chars".to_string(),
                false
            )]
        );

        let event = serde_json::json!({
            "type": "item.completed",
            "item": {"type": "agent_message", "text": "secret-looking text"}
        });
        let out = describe_line(Provider::Codex, event.to_string().as_bytes(), &redactor);
        assert_eq!(
            out,
            [(
                "item.completed".to_string(),
                "message: 19 chars".to_string(),
                false
            )]
        );
    }

    #[test]
    fn an_error_text_is_cut_and_has_credentials_removed() {
        let redactor = Redactor::default();
        let long = serde_json::json!({"type": "error", "message": "word ".repeat(100)});
        let out = describe_line(Provider::Claude, long.to_string().as_bytes(), &redactor);
        assert!(
            out[0].1.chars().count() <= PROGRESS_SUMMARY_CHARS + 3,
            "{out:?}"
        );

        let secret = serde_json::json!({
            "type": "error",
            "message": "failed with sk-ant-abcdefghijklmnopqrstuvwx"
        });
        let out = describe_line(Provider::Claude, secret.to_string().as_bytes(), &redactor);
        assert!(!out[0].1.contains("sk-ant-"), "{out:?}");
    }

    #[test]
    fn an_event_type_is_cut_and_has_credentials_removed_in_the_kind() {
        let redactor = Redactor::default();
        for provider in [Provider::Claude, Provider::Codex] {
            let secret = serde_json::json!({"type": "leak=sk-ant-abcdefghijklmnopqrstuvwx"});
            let out = describe_line(provider, secret.to_string().as_bytes(), &redactor);
            assert_eq!(out[0].0, "leak=[redacted]", "{out:?}");

            let long = serde_json::json!({"type": "k".repeat(MAX_LINE_BYTES / 2)});
            let out = describe_line(provider, long.to_string().as_bytes(), &redactor);
            assert_eq!(
                out[0].0,
                format!("{}...", "k".repeat(PROGRESS_KIND_CHARS)),
                "{provider:?}"
            );
        }
    }

    #[test]
    fn a_configured_value_with_inner_whitespace_is_redacted_in_kind_and_summary() {
        let value = "fixture\tprivate  token";
        let redactor = Redactor::new([value.to_string()]);
        let event = serde_json::json!({
            "type": format!("t {value} t"),
            "subtype": format!("s {value} s"),
            "message": format!("m {value} m")
        });
        for provider in [Provider::Claude, Provider::Codex] {
            let out = describe_line(provider, event.to_string().as_bytes(), &redactor);
            assert_eq!(out[0].0, "t [redacted] t", "{provider:?}");
            assert!(!out[0].0.contains("private"), "{out:?}");
        }
        for kind in ["error", "system"] {
            let mut event = event.clone();
            event["type"] = serde_json::json!(kind);
            let out = describe_line(Provider::Claude, event.to_string().as_bytes(), &redactor);
            assert!(out[0].1.contains("[redacted]"), "{out:?}");
            assert!(!out[0].1.contains("private"), "{out:?}");
        }
    }

    #[test]
    fn a_configured_value_across_the_kind_cut_leaves_no_part_of_itself() {
        let value = "fixture-private-token";
        let redactor = Redactor::new([value.to_string()]);
        // The value starts at character 35 and would run past the 40-character
        // cut; the mask that replaces it is cut instead.
        let kind = format!("{}{value}", "k".repeat(35));
        let event = serde_json::json!({"type": kind});
        let out = describe_line(Provider::Codex, event.to_string().as_bytes(), &redactor);
        assert_eq!(out[0].0, format!("{}[reda...", "k".repeat(35)));
        assert!(!out[0].0.contains("fixture"), "{out:?}");
    }

    #[test]
    fn only_a_type_that_is_exactly_system_is_marked_as_system() {
        let redactor = Redactor::default();
        for (kind, system) in [("system", true), (" system ", false), ("system\n", false)] {
            let event = serde_json::json!({"type": kind, "subtype": "thinking_tokens"});
            let out = describe_line(Provider::Claude, event.to_string().as_bytes(), &redactor);
            assert_eq!(out[0].0, "system", "{kind:?}");
            assert_eq!(out[0].2, system, "{kind:?}");
        }
    }

    #[test]
    fn real_event_types_are_reported_unchanged_as_kinds() {
        let redactor = Redactor::default();
        let claude = [
            "assistant",
            "user",
            "system",
            "result",
            "error",
            "rate_limit_event",
            "stream_event",
        ];
        for kind in claude {
            let event = serde_json::json!({"type": kind});
            let out = describe_line(Provider::Claude, event.to_string().as_bytes(), &redactor);
            assert_eq!(out[0].0, kind);
        }
        let codex = [
            "thread.started",
            "turn.started",
            "item.started",
            "item.updated",
            "item.completed",
            "turn.completed",
            "turn.failed",
            "error",
        ];
        for kind in codex {
            let event = serde_json::json!({"type": kind});
            let out = describe_line(Provider::Codex, event.to_string().as_bytes(), &redactor);
            assert_eq!(out[0].0, kind);
        }
    }

    #[test]
    fn a_command_is_never_put_in_a_summary() {
        let event = serde_json::json!({
            "type": "item.completed",
            "item": {"type": "command_execution", "command": "echo SECRET-TEXT", "status": "completed"}
        });
        let out = describe_line(
            Provider::Codex,
            event.to_string().as_bytes(),
            &Redactor::default(),
        );
        assert_eq!(
            out,
            [(
                "item.completed".to_string(),
                "command_execution completed".to_string(),
                false
            )]
        );
    }

    #[test]
    fn message_texts_have_credentials_removed_and_are_cut() {
        let redactor = Redactor::default();
        let event = serde_json::json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "text", "text": "key sk-ant-abcdefghijklmnopqrstuvwx here"},
                {"type": "tool_use", "name": "Bash"},
                {"type": "text", "text": ""}
            ]}
        });
        let out = message_texts(Provider::Claude, event.to_string().as_bytes(), &redactor);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(!out[0].contains("sk-ant-"), "{out:?}");

        let long = serde_json::json!({
            "type": "item.completed",
            "item": {"type": "agent_message", "text": "w".repeat(PROGRESS_TEXT_CHARS * 2)}
        });
        let out = message_texts(Provider::Codex, long.to_string().as_bytes(), &redactor);
        assert_eq!(out[0].chars().count(), PROGRESS_TEXT_CHARS + 3);

        let started = serde_json::json!({
            "type": "item.started",
            "item": {"type": "agent_message", "text": "draft"}
        });
        assert!(
            message_texts(Provider::Codex, started.to_string().as_bytes(), &redactor).is_empty()
        );
    }

    #[test]
    fn a_line_that_is_not_json_stands_for_no_event() {
        assert!(describe_line(Provider::Codex, b"plain text", &Redactor::default()).is_empty());
    }
}

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
//! A summary carries at most [`PROGRESS_SUMMARY_CHARS`] characters of a
//! message, with credentials removed, and never a command, a path or an
//! environment value.

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Map, Value};

use crate::contract::outcome::Outcome;
use crate::contract::request::Provider;
use crate::contract::result::RunResult;
use crate::process::OutputTap;

use super::limits::{MAX_LINE_BYTES, PROGRESS_SUMMARY_CHARS};
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
        /// `item.completed`.
        kind: String,
        /// A short description, possibly empty.
        summary: String,
    },
    /// A request the provider made was answered no.
    Denied {
        /// What the provider asked to do, as in `permissions.denied`.
        request: String,
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
pub(super) struct Progress(Option<ProgressSink>);

impl Progress {
    pub fn new(sink: Option<ProgressSink>) -> Self {
        Self(sink)
    }

    pub fn emit(&self, event: ProgressEvent) {
        if let Some(sink) = &self.0 {
            sink(event);
        }
    }

    /// Announce the denials and the end of a run.
    pub fn finished(&self, result: &RunResult) {
        if self.0.is_none() {
            return;
        }
        for denied in &result.permissions.denied {
            self.emit(ProgressEvent::Denied {
                request: denied.request.clone(),
            });
        }
        self.emit(ProgressEvent::Finished {
            outcome: result.outcome,
        });
    }

    /// A tap for the provider's standard output that turns each line into
    /// events, or `None` when nobody listens.
    pub fn tap(&self, provider: Provider, redactor: Redactor) -> Option<OutputTap> {
        self.0.as_ref()?;
        let progress = self.clone();
        let framer = Mutex::new(Framer::default());
        Some(Arc::new(move |chunk: &[u8]| {
            let lines = framer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .feed(chunk);
            for line in lines {
                for (kind, summary) in describe_line(provider, &line, &redactor) {
                    progress.emit(ProgressEvent::Provider { kind, summary });
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
/// stands for none.
fn describe_line(provider: Provider, line: &[u8], redactor: &Redactor) -> Vec<(String, String)> {
    let Ok(Value::Object(event)) = serde_json::from_slice::<Value>(line.trim_ascii()) else {
        return Vec::new();
    };
    let described = match provider {
        Provider::Claude => describe_claude(&event),
        Provider::Codex => describe_codex(&event),
    };
    described
        .into_iter()
        .map(|(kind, summary)| (kind, short(&summary, redactor)))
        .collect()
}

/// One line, with credentials removed, cut to the summary length.
fn short(text: &str, redactor: &Redactor) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    clip(&redactor.apply(&one_line), PROGRESS_SUMMARY_CHARS)
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
                        Some(("assistant".to_string(), format!("message: {text}")))
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
                format!("message: {text}")
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
    fn an_answer_is_cut_to_the_summary_length_and_credentials_are_removed() {
        let redactor = Redactor::default();
        let long = "word ".repeat(100);
        let event = serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": long}]}
        });
        let out = describe_line(Provider::Claude, event.to_string().as_bytes(), &redactor);
        assert_eq!(out.len(), 1);
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
                "command_execution completed".to_string()
            )]
        );
    }

    #[test]
    fn a_line_that_is_not_json_stands_for_no_event() {
        assert!(describe_line(Provider::Codex, b"plain text", &Redactor::default()).is_empty());
    }
}

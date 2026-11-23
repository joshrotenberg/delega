//! The Claude Code permission control protocol, adapted from Due's Claude
//! session and its claude-wrapper 0.14.11 duplex transport.

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::contract::result::{AskedPermission, PermissionDecision, PermissionInteraction};
use crate::process::dialogue::{Dialogue, End};

use super::redact::Redactor;

const FIELD_BYTES: usize = 1024;
const MAX_REQUEST_ID_BYTES: usize = 256;
const MAX_CONTROL_FRAME_BYTES: usize = 256 * 1024;

pub(super) fn user_line(prompt: &[u8]) -> Vec<u8> {
    line(&json!({
        "type": "user",
        "message": {"role": "user", "content": String::from_utf8_lossy(prompt)},
        "parent_tool_use_id": null,
    }))
}

pub(super) struct PermissionRequest<'a> {
    pub id: &'a str,
    pub tool: &'a str,
    pub input: &'a Value,
    description: Option<&'a str>,
}

impl<'a> PermissionRequest<'a> {
    pub fn parse(frame: &'a Value) -> Option<Self> {
        if frame.get("type")?.as_str()? != "control_request" {
            return None;
        }
        let request = frame.get("request")?;
        if request.get("subtype")?.as_str()? != "can_use_tool" {
            return None;
        }
        let input = request.get("input")?;
        input.as_object()?;
        let id = frame.get("request_id")?.as_str()?;
        if id.is_empty() || id.len() > MAX_REQUEST_ID_BYTES {
            return None;
        }
        Some(Self {
            id,
            tool: request.get("tool_name")?.as_str()?,
            input,
            description: request.get("description").and_then(Value::as_str),
        })
    }

    pub fn response(&self, deny: Option<&str>) -> Vec<u8> {
        let response = match deny {
            Some(message) => json!({"behavior": "deny", "message": message}),
            None => json!({"behavior": "allow", "updatedInput": self.input}),
        };
        line(&json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": self.id, "response": response}
        }))
    }

    pub fn summary(&self, redactor: &Redactor) -> Value {
        let mut summary = serde_json::Map::new();
        let mut truncated = false;
        let (text, sizes): (&[&str], &[&str]) = match self.tool {
            "Read" | "Glob" | "Grep" => (&["file_path", "path", "pattern"], &[]),
            "Bash" => (&["command", "description"], &[]),
            "Write" => (&["file_path"], &["content"]),
            "Edit" => (&["file_path"], &["old_string", "new_string"]),
            "NotebookEdit" => (&["notebook_path", "file_path"], &["new_source"]),
            "WebFetch" => (&["url"], &[]),
            "WebSearch" => (&["query"], &[]),
            _ => {
                let mut keys = self
                    .input
                    .as_object()
                    .into_iter()
                    .flat_map(|input| input.keys());
                let fields: Vec<_> = keys
                    .by_ref()
                    .take(16)
                    .map(|key| field(key, redactor, &mut truncated))
                    .collect();
                let mut summary = json!({"keys": fields});
                if truncated || keys.next().is_some() {
                    summary["truncated"] = json!(true);
                }
                return summary;
            }
        };
        for key in text {
            if let Some(value) = self.input.get(key).and_then(Value::as_str).or_else(|| {
                (*key == "description")
                    .then_some(self.description)
                    .flatten()
            }) {
                summary.insert((*key).into(), json!(field(value, redactor, &mut truncated)));
            }
        }
        for key in sizes {
            if let Some(value) = self.input.get(key).and_then(Value::as_str) {
                summary.insert(format!("{key}_bytes"), json!(value.len()));
            }
        }
        if truncated {
            summary.insert("truncated".into(), json!(true));
        }
        Value::Object(summary)
    }
}

fn field(value: &str, redactor: &Redactor, truncated: &mut bool) -> String {
    let mut value = redactor.apply(value);
    if value.len() > FIELD_BYTES {
        *truncated = true;
        let mut end = FIELD_BYTES - " [truncated]".len();
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
        value.push_str(" [truncated]");
    }
    value
}

fn line(value: &Value) -> Vec<u8> {
    let mut bytes = value.to_string().into_bytes();
    bytes.push(b'\n');
    bytes
}

const MAX_PENDING: usize = 32;
const MAX_REQUESTS: usize = 256;

struct Entry {
    view: PermissionInteraction,
    deadline: Instant,
    native: Option<Value>,
    answered: Option<AskedPermission>,
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    responses: Vec<(usize, Vec<u8>)>,
    closed_by: Option<&'static str>,
    failed: bool,
    run_deadline: Option<Instant>,
}

/// Internal endpoint installed by the supervisor.
#[derive(Clone, Default)]
pub struct AskEndpoint(pub(crate) Option<Arc<Asks>>);

pub(crate) struct Asks {
    state: Mutex<State>,
    run: String,
    timeout: Duration,
    cancel: CancellationToken,
    timers: CancellationToken,
    writer: Notify,
    changed: Box<dyn Fn() + Send + Sync>,
    weak: Weak<Self>,
    redactor: Mutex<Redactor>,
}

impl Asks {
    pub(crate) fn new(
        run: String,
        timeout: u64,
        cancel: CancellationToken,
        changed: impl Fn() + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            run,
            timeout: Duration::from_secs(timeout),
            cancel,
            changed: Box::new(changed),
            weak: weak.clone(),
            timers: CancellationToken::new(),
            writer: Notify::new(),
            state: Mutex::new(State::default()),
            redactor: Mutex::new(Redactor::default()),
        })
    }

    pub(super) fn redact_with(&self, redactor: Redactor) {
        *self.redactor.lock().unwrap() = redactor;
    }

    pub(crate) fn pending(&self) -> Vec<PermissionInteraction> {
        self.state
            .lock()
            .unwrap()
            .entries
            .iter()
            .filter(|entry| entry.answered.is_none())
            .map(|entry| entry.view.clone())
            .collect()
    }

    pub(crate) fn audit(&self) -> Vec<AskedPermission> {
        self.state
            .lock()
            .unwrap()
            .entries
            .iter()
            .filter_map(|entry| entry.answered.clone())
            .collect()
    }

    pub(super) fn closed_by(&self) -> Option<&'static str> {
        self.state.lock().unwrap().closed_by
    }

    pub(super) fn failed(&self) -> bool {
        self.state.lock().unwrap().failed
    }

    pub(crate) fn respond(
        &self,
        id: &str,
        choice: PermissionDecision,
    ) -> (&'static str, Option<AskedPermission>) {
        let mut state = self.state.lock().unwrap();
        let Some(index) = state.entries.iter().position(|entry| entry.view.id == id) else {
            return ("unknown_interaction", None);
        };
        if let Some(answer) = &state.entries[index].answered {
            return (resolved_status(answer), Some(answer.clone()));
        }
        let now = Instant::now();
        let deadline = state.entries[index].deadline;
        if state
            .run_deadline
            .is_some_and(|run| run <= now && run <= deadline)
        {
            self.resolve(&mut state, index, PermissionDecision::Deny, "run_timeout");
            return ("closed", state.entries[index].answered.clone());
        }
        let expired = now >= deadline;
        let (choice, reason, status) = if expired {
            (PermissionDecision::Deny, "timeout", "timed_out")
        } else {
            (choice, "caller", "accepted")
        };
        self.resolve(&mut state, index, choice, reason);
        (status, state.entries[index].answered.clone())
    }

    fn resolve(&self, state: &mut State, index: usize, decision: PermissionDecision, reason: &str) {
        let entry = &mut state.entries[index];
        let Some(frame) = entry.native.take() else {
            return;
        };
        let request = PermissionRequest::parse(&frame).expect("validated permission request");
        let deny = (decision == PermissionDecision::Deny).then_some(match reason {
            "caller" => "Permission denied by the caller.",
            "timeout" => "Permission request timed out.",
            "request_too_large" => "Permission request exceeded the size limit.",
            _ => "Permission denied because the run is stopping.",
        });
        state.responses.push((index, request.response(deny)));
        entry.answered = Some(AskedPermission {
            interaction: entry.view.clone(),
            decision,
            decided_by: reason.to_owned(),
            written: false,
        });
        self.writer.notify_one();
        (self.changed)();
    }

    pub(crate) fn close(&self, reason: &'static str) {
        let mut state = self.state.lock().unwrap();
        if state.closed_by.is_some() {
            return;
        }
        state.closed_by = Some(reason);
        self.timers.cancel();
        for index in 0..state.entries.len() {
            self.resolve(&mut state, index, PermissionDecision::Deny, reason);
        }
        self.writer.notify_one();
    }

    fn fail(&self) {
        self.state.lock().unwrap().failed = true;
        self.close("protocol_error");
        self.cancel.cancel();
    }
}

impl Dialogue for Asks {
    fn started(&self, deadline: Instant) {
        self.state.lock().unwrap().run_deadline = Some(deadline);
    }

    fn line(&self, line: &[u8]) -> bool {
        if line.iter().all(u8::is_ascii_whitespace) {
            return true;
        }
        let Ok(frame) = serde_json::from_slice::<Value>(line) else {
            return true;
        };
        match frame.get("type").and_then(Value::as_str) {
            Some("result") if frame.get("parent_tool_use_id").is_none_or(Value::is_null) => {
                self.close("provider_finished");
                true
            }
            Some("control_request") => {
                if self.state.lock().unwrap().closed_by.is_some() {
                    return false;
                }
                let oversized = line.len() > MAX_CONTROL_FRAME_BYTES;
                let Some(request) = PermissionRequest::parse(&frame) else {
                    self.fail();
                    return false;
                };
                let mut state = self.state.lock().unwrap();
                if state.closed_by.is_some() {
                    return false;
                }
                let index = state.entries.len();
                if index >= MAX_REQUESTS
                    || state
                        .entries
                        .iter()
                        .filter_map(|entry| entry.native.as_ref())
                        .any(|frame| frame["request_id"].as_str() == Some(request.id))
                {
                    drop(state);
                    self.fail();
                    return false;
                }
                let known = super::claude::BUILTINS.contains(&request.tool);
                let view = PermissionInteraction {
                    id: format!("{}:{}", self.run, index + 1),
                    tool: if known {
                        request.tool.into()
                    } else {
                        "unknown".into()
                    },
                    summary: if oversized {
                        json!({"truncated": true})
                    } else {
                        request.summary(&self.redactor.lock().unwrap())
                    },
                };
                let blocked = !known
                    || (request.tool == "Bash"
                        && ["run_in_background", "dangerouslyDisableSandbox"]
                            .iter()
                            .any(|key| {
                                request
                                    .input
                                    .get(key)
                                    .is_some_and(|v| v != &Value::Bool(false) && !v.is_null())
                            }));
                let full = state
                    .entries
                    .iter()
                    .filter(|entry| entry.native.is_some())
                    .count()
                    >= MAX_PENDING;
                let deadline = Instant::now() + self.timeout;
                state.entries.push(Entry {
                    view,
                    deadline,
                    native: Some(frame),
                    answered: None,
                });
                let reason = if oversized {
                    Some("request_too_large")
                } else if blocked {
                    Some("tool_restricted")
                } else if full {
                    Some("pending_limit")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    self.resolve(&mut state, index, PermissionDecision::Deny, reason);
                } else {
                    (self.changed)();
                    let weak = self.weak.clone();
                    let timers = self.timers.clone();
                    tokio::spawn(async move {
                        tokio::select! {
                            () = timers.cancelled() => return,
                            () = tokio::time::sleep_until(deadline) => {}
                        }
                        if let Some(asks) = weak.upgrade() {
                            let mut state = asks.state.lock().unwrap();
                            let reason = if state.run_deadline.is_some_and(|run| run <= deadline) {
                                "run_timeout"
                            } else {
                                "timeout"
                            };
                            asks.resolve(&mut state, index, PermissionDecision::Deny, reason);
                        }
                    });
                }
                false
            }
            Some("control_cancel_request") => {
                if let Some(id) = frame.get("request_id").and_then(Value::as_str) {
                    let mut state = self.state.lock().unwrap();
                    if let Some(entry) = state.entries.iter_mut().find(|entry| {
                        entry
                            .native
                            .as_ref()
                            .is_some_and(|request| request["request_id"].as_str() == Some(id))
                    }) {
                        entry.native = None;
                        entry.answered = Some(AskedPermission {
                            interaction: entry.view.clone(),
                            decision: PermissionDecision::Deny,
                            decided_by: "provider_cancelled".to_owned(),
                            written: false,
                        });
                        (self.changed)();
                    }
                }
                false
            }
            Some("control_response") => false,
            _ => true,
        }
    }

    fn changed(&self) -> &Notify {
        &self.writer
    }

    fn responses(&self) -> (Vec<(usize, Vec<u8>)>, bool) {
        let mut state = self.state.lock().unwrap();
        (
            std::mem::take(&mut state.responses),
            state.closed_by.is_some(),
        )
    }

    fn written(&self, key: usize) {
        if let Some(answer) = &mut self.state.lock().unwrap().entries[key].answered {
            answer.written = true;
        }
    }

    fn end(&self, reason: End) {
        match reason {
            End::Interrupted => self.close("interrupt"),
            End::TimedOut => self.close("run_timeout"),
            End::ProviderClosed => self.close("provider_finished"),
            End::ProtocolError => self.fail(),
        }
    }
}

pub(crate) fn resolved_status(answer: &AskedPermission) -> &'static str {
    match answer.decided_by.as_str() {
        "timeout" => "timed_out",
        "caller" => "already_answered",
        _ => "closed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interaction_redacts_text_before_truncating_and_omits_file_content() {
        let redactor = Redactor::new(["fixture-private-token".to_owned()]);
        let bash = json!({
            "type": "control_request", "request_id": "native-bash",
            "request": {
                "subtype": "can_use_tool", "tool_name": "Bash",
                "description": "fixture-private-token",
                "input": {"command": format!("{}fixture-private-token", "é".repeat(510))}
            }
        });
        let view = PermissionRequest::parse(&bash).unwrap().summary(&redactor);
        assert_eq!(view["description"], "[redacted]");
        assert_eq!(view["truncated"], true);
        let command = view["command"].as_str().unwrap();
        assert!(command.len() <= FIELD_BYTES && command.ends_with(" [truncated]"));
        assert!(!view.to_string().contains("fixture-private-token"));

        let write = json!({
            "type": "control_request", "request_id": "native-write",
            "request": {
                "subtype": "can_use_tool", "tool_name": "Write",
                "input": {"content": "unpublished text", "file_path": "test.txt"}
            }
        });
        let view = PermissionRequest::parse(&write).unwrap().summary(&redactor);
        assert_eq!(view, json!({"file_path": "test.txt", "content_bytes": 16}));
    }

    #[tokio::test]
    async fn out_of_order_decisions_preserve_native_ids_and_only_one_reply_wins() {
        let asks = Asks::new("test-run".to_owned(), 120, CancellationToken::new(), || {});
        for native_id in ["native-one", "native-two"] {
            let frame = json!({
                "type": "control_request",
                "request_id": native_id,
                "request": {"subtype": "can_use_tool", "tool_name": "Read", "input": {"file_path": "fixture"}}
            });
            assert!(!asks.line(&line(&frame)));
        }
        let pending = asks.pending();
        assert_eq!(pending.len(), 2);
        let second = pending[1].id.clone();
        let allowed = asks.respond(&second, PermissionDecision::Allow);
        assert_eq!(allowed.0, "accepted");
        let duplicate = asks.respond(&second, PermissionDecision::Deny);
        assert_eq!(duplicate.0, "already_answered");
        assert_eq!(duplicate.1.unwrap().decision, PermissionDecision::Allow);

        asks.close("close");
        let (replies, close) = asks.responses();
        assert!(close);
        let responses = replies
            .iter()
            .map(|(_, bytes)| serde_json::from_slice::<Value>(bytes).unwrap()["response"].clone())
            .collect::<Vec<_>>();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["request_id"], "native-two");
        assert_eq!(responses[0]["response"]["behavior"], "allow");
        assert_eq!(responses[1]["request_id"], "native-one");
        assert_eq!(responses[1]["response"]["behavior"], "deny");
    }

    fn permission_frame(id: &str, tool: &str, input: Value) -> Vec<u8> {
        line(&json!({
            "type":"control_request", "request_id":id,
            "request":{"subtype":"can_use_tool", "tool_name":tool, "input":input}
        }))
    }

    #[test]
    fn read_and_search_summaries_show_redacted_paths_and_patterns() {
        let redactor = Redactor::new(["fixture-private-token".to_owned()]);
        for (tool, input, expected) in [
            (
                "Read",
                json!({"file_path":"/fixture/fixture-private-token"}),
                json!({"file_path":"/fixture/[redacted]"}),
            ),
            (
                "Glob",
                json!({"path":"/fixture","pattern":"*.rs"}),
                json!({"path":"/fixture","pattern":"*.rs"}),
            ),
            (
                "Grep",
                json!({"path":"/fixture","pattern":"fixture-private-token"}),
                json!({"path":"/fixture","pattern":"[redacted]"}),
            ),
        ] {
            let bytes = permission_frame("native", tool, input);
            let frame = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                PermissionRequest::parse(&frame).unwrap().summary(&redactor),
                expected
            );
        }
    }

    #[tokio::test]
    async fn background_sandbox_bypass_and_unknown_tools_are_denied_without_a_pending_interaction()
    {
        let asks = Asks::new("run".into(), 120, CancellationToken::new(), || {});
        for (id, tool, input) in [
            (
                "background",
                "Bash",
                json!({"command":"ls","run_in_background":true}),
            ),
            (
                "bypass",
                "Bash",
                json!({"command":"ls","dangerouslyDisableSandbox":true}),
            ),
            ("unknown", "UnknownTool", json!({})),
        ] {
            assert!(!asks.line(&permission_frame(id, tool, input)));
            assert!(asks.pending().is_empty());
            let (replies, closed) = asks.responses();
            assert!(!closed);
            assert_eq!(replies.len(), 1);
            let reply: Value = serde_json::from_slice(&replies[0].1).unwrap();
            assert_eq!(reply["response"]["request_id"], id);
            assert_eq!(reply["response"]["response"]["behavior"], "deny");
        }
        assert_eq!(asks.audit().len(), 3);
        assert!(
            asks.audit()
                .iter()
                .all(|entry| entry.decided_by == "tool_restricted")
        );
    }

    #[tokio::test]
    async fn the_pending_cap_denies_overflow_and_the_lifetime_cap_closes_the_dialogue() {
        let cancel = CancellationToken::new();
        let asks = Asks::new("run".into(), 120, cancel.clone(), || {});
        for i in 0..MAX_REQUESTS {
            assert!(!asks.line(&permission_frame(&i.to_string(), "Read", json!({}))));
        }
        assert_eq!(asks.pending().len(), MAX_PENDING);
        assert_eq!(asks.audit().len(), MAX_REQUESTS - MAX_PENDING);
        assert!(
            asks.audit()
                .iter()
                .all(|entry| entry.decided_by == "pending_limit")
        );
        assert!(!cancel.is_cancelled());

        assert!(!asks.line(&permission_frame("overflow", "Read", json!({}))));
        assert!(cancel.is_cancelled());
        assert!(asks.failed());
        assert!(asks.pending().is_empty());
        assert_eq!(asks.audit().len(), MAX_REQUESTS);
        let (responses, closed) = asks.responses();
        assert!(closed);
        assert_eq!(responses.len(), MAX_REQUESTS);
    }

    #[tokio::test]
    async fn provider_cancel_retires_the_request_without_writing_a_reply() {
        let asks = Asks::new("run".into(), 120, CancellationToken::new(), || {});
        assert!(!asks.line(&permission_frame("native", "Read", json!({}))));
        let pending = asks.pending().pop().unwrap();
        assert!(!asks.line(&line(
            &json!({"type":"control_cancel_request","request_id":"native"})
        )));
        assert!(asks.pending().is_empty());
        assert_eq!(
            asks.respond(&pending.id, PermissionDecision::Allow).0,
            "closed"
        );
        assert!(asks.responses().0.is_empty());
        let answer = asks.audit().pop().unwrap();
        assert_eq!(answer.decided_by, "provider_cancelled");
        assert!(!answer.written);
    }

    #[test]
    fn non_json_stdout_is_kept_for_the_ordinary_diagnostics() {
        let asks = Asks::new("run".into(), 120, CancellationToken::new(), || {});
        assert!(asks.line(b"{ broken JSON\n"));
        assert!(asks.line(b"not JSON\n"));
        assert!(!asks.failed());
    }
}

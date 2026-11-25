//! Codex CLI: launch arguments and output stream.
//!
//! Commands run as `codex exec --json`, the prompt on standard input. Sandbox
//! and approval policy carry the permission mapping, which is explained in
//! `docs/providers.md`.
//!
//! Disable login shells so the user's shell profile cannot add environment
//! variables to worker commands. Keep Codex's default environment inheritance:
//! setting `shell_environment_policy.inherit="core"` would also remove some
//! variables the caller explicitly passed with `--env`.

use serde_json::{Map, Value};

use crate::contract::permissions::Mode;
use crate::contract::request::Effort;
use crate::contract::result::Usage;

use super::launch::{Built, Elide, Inputs};
use super::refusal::Refusal;
use super::stream::{Parsed, Terminal, count, for_each_event, non_empty, str_field};

/// Features switched off for every worker.
const DISABLED_FEATURES: [&str; 4] = ["plugins", "apps", "hooks", "multi_agent"];

/// Shown in a dry run in place of the MCP override, which is built from the
/// servers the installed Codex lists when the run starts.
pub(super) const MCP_PLACEHOLDER: &str =
    "mcp_servers=<one disabled entry per configured server, listed at start>";

fn effort(e: Effort) -> &'static str {
    match e {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
    }
}

/// A TOML basic string holding `text`. JSON string syntax is valid TOML here:
/// `"` and `\` are escaped, as are control characters (`\n`, `\t` and the
/// like, others as `\u00XX`); everything else is kept as UTF-8.
fn toml_string(text: &str) -> String {
    Value::from(text).to_string()
}

/// The `-c` argument that carries the system text to Codex.
fn instructions_argument(system: &str) -> String {
    format!("developer_instructions={}", toml_string(system))
}

/// The length in bytes of the argument that carries `system`, after the key is
/// added and the text is TOML-encoded.
pub(super) fn instructions_argument_len(system: &str) -> usize {
    instructions_argument(system).len()
}

fn sandbox(mode: Mode) -> &'static str {
    match mode {
        Mode::Read => "read-only",
        Mode::Write | Mode::Build => "workspace-write",
        Mode::Full => "danger-full-access",
    }
}

/// Build the launch for a request. The MCP override argument is left empty at
/// `mcp_index` until discovery has run.
pub(super) fn build(inputs: &Inputs<'_>) -> Result<Built, Refusal> {
    let request = inputs.request;
    let scope = inputs.scope;
    let sandbox_mode = sandbox(scope.mode);

    let mut args: Vec<String> = DISABLED_FEATURES
        .iter()
        .flat_map(|f| ["--disable".to_string(), (*f).to_string()])
        .collect();
    let mut settings = vec![
        "approval_policy=\"never\"".to_string(),
        format!("sandbox_mode={}", toml_string(sandbox_mode)),
        // Commands Codex runs start no login shell, so they do not load the
        // user's shell profile and see only the environment delega passed.
        "allow_login_shell=false".to_string(),
    ];
    // `write` keeps the temporary directories out of the sandbox; `build`
    // lets commands such as test runners create files there.
    if matches!(scope.mode, Mode::Write | Mode::Build) {
        let exclude_tmp = scope.mode == Mode::Write;
        settings.extend([
            "sandbox_workspace_write.network_access=false".to_string(),
            format!("sandbox_workspace_write.exclude_slash_tmp={exclude_tmp}"),
            format!("sandbox_workspace_write.exclude_tmpdir_env_var={exclude_tmp}"),
        ]);
    }
    if let Some(e) = request.effort {
        settings.push(format!("model_reasoning_effort={}", toml_string(effort(e))));
    }
    for setting in &settings {
        args.extend(["-c".to_string(), setting.clone()]);
    }
    args.push("-c".to_string());
    let instructions_index = args.len();
    args.push(instructions_argument(inputs.system));
    args.push("-c".to_string());
    let mcp_index = args.len();
    args.push(String::new());
    args.extend(inputs.native.iter().cloned());

    args.push("exec".to_string());
    if let Some(id) = &request.session.resume {
        args.extend(["resume".to_string(), id.clone()]);
    }
    args.extend(["--ignore-user-config", "--json", "--skip-git-repo-check"].map(String::from));
    if request.session.ephemeral {
        args.push("--ephemeral".to_string());
    }
    if let Some(model) = &request.model {
        args.extend(["--model".to_string(), model.clone()]);
    }
    args.push("-".to_string());

    let mut notes = vec![
        "Codex does not report denials in its event stream except commands it marks declined; \
         other blocked actions show up as failed commands and are not listed in permissions.denied"
            .to_string(),
        "Codex user configuration is ignored (--ignore-user-config), and plugins, apps, hooks and \
         multi-agent are disabled; Codex may still load its own AGENTS.md files"
            .to_string(),
    ];
    match scope.mode {
        Mode::Read => notes.push(
            "wider than requested: the read-only sandbox lets Codex read any file this user can \
             read, including outside the working directory and permissions.directories, and run \
             any command that needs no write access and no network access; permissions.commands \
             is not enforced as a list and permissions.directories does not limit reading"
                .to_string(),
        ),
        Mode::Write => notes.push(
            "wider than requested: the workspace-write sandbox lets Codex read any file this user \
             can read, including outside the working directory and permissions.directories, and \
             run any command that works inside the sandbox (writes only in the working directory, \
             no network, no temporary directories); permissions.commands is not enforced as a \
             list and permissions.directories does not limit reading"
                .to_string(),
        ),
        Mode::Build => notes.extend([
            "wider than requested: the workspace-write sandbox lets Codex read any file this user \
             can read, including outside the working directory and permissions.directories, and \
             run any command that works inside the sandbox (writes in the working directory and \
             under /tmp and $TMPDIR, including a permissions.directories entry there, except \
             .git; no network; on macOS a live probe found /bin/ps refused inside the \
             sandbox); \
             permissions.directories does not limit reading"
                .to_string(),
            "the workspace-write sandbox keeps .git unwritable, so git add and git commit fail; \
             commit the worker's changes yourself"
                .to_string(),
        ]),
        Mode::Full => notes.push("full runs Codex with no sandbox".to_string()),
    }
    let mut unenforceable = Vec::new();
    if scope.mode != Mode::Full {
        unenforceable.push(
            "reads are not confined to the working directory and permissions.directories"
                .to_string(),
        );
        match scope.mode {
            Mode::Write => unenforceable.push(
                "commands that write inside the working directory or need no write access and no \
                 network access can run although the request allows only the commands it lists"
                    .to_string(),
            ),
            Mode::Read => unenforceable.push(
                "commands that need no write access and no network access can run although the \
                 request allows only the commands it lists"
                    .to_string(),
            ),
            // `build` lets every command run inside the sandbox, as requested.
            Mode::Build | Mode::Full => {}
        }
        if !scope.commands.is_empty() {
            unenforceable.push("permissions.commands has no effect".to_string());
        }
    }
    let mut mapping = settings.clone();
    mapping.push("developer_instructions=<system prompt>".to_string());
    mapping.push("features disabled=plugins,apps,hooks,multi_agent".to_string());
    Ok(Built {
        args,
        env: Vec::new(),
        notes,
        unenforceable,
        settings: mapping,
        commands: Vec::new(),
        elide: vec![Elide {
            index: instructions_index,
            prefix: "developer_instructions=",
        }],
        needs_hook_version: false,
        mcp_index: Some(mcp_index),
    })
}

/// The `mcp_servers` override that disables every server `codex mcp list
/// --json` reported.
pub(super) fn disabled_servers(stdout: &str) -> Result<String, String> {
    let servers: Value = serde_json::from_str(stdout)
        .map_err(|_| "Codex MCP discovery returned invalid JSON".to_string())?;
    let list = servers
        .as_array()
        .ok_or_else(|| "Codex MCP discovery returned something other than a list".to_string())?;
    let mut entries = Vec::new();
    for server in list {
        let Some(name) = server.get("name").and_then(Value::as_str) else {
            return Err("Codex MCP discovery listed a server without a name".to_string());
        };
        let field = match server
            .get("transport")
            .and_then(|t| t.get("type"))
            .and_then(Value::as_str)
        {
            Some("stdio") => "command=\"__delega_disabled__\"",
            Some("streamable_http") => "url=\"http://127.0.0.1:9\"",
            _ => {
                return Err("Codex MCP discovery listed a server with a transport that \
                            cannot be disabled"
                    .to_string());
            }
        };
        entries.push(format!("{}={{enabled=false,{field}}}", toml_string(name)));
    }
    Ok(format!("mcp_servers={{{}}}", entries.join(",")))
}

/// Usage from a `turn.completed` or `turn.failed` event. Codex reports the
/// thread's totals, so they are the session's, and the run's own only when the
/// run did not resume the thread. The run's own figures on a resumed thread
/// are not reported.
fn usages(event: &Map<String, Value>, parsed: &mut Parsed) {
    let session = event.get("usage").and_then(|counts| {
        non_empty(Usage {
            input_tokens: count(counts, "input_tokens"),
            output_tokens: count(counts, "output_tokens"),
            cached_tokens: count(counts, "cached_input_tokens"),
            reasoning_tokens: count(counts, "reasoning_output_tokens"),
            cost_usd: None,
        })
    });
    parsed.usage = None;
    parsed.fresh_usage = session.clone();
    parsed.session_usage = session;
}

/// The error kind a `codex_error_info` name (see [`info_name`]) stands for,
/// when it is one delega knows.
fn info_kind(name: &str) -> Option<&'static str> {
    match name {
        "usage_limit_exceeded" => Some("provider_usage_limit"),
        "context_window_exceeded" => Some("provider_context_exhausted"),
        _ => None,
    }
}

/// The error kind of a message that is itself a JSON error document, from
/// its `error.code` or `error.type` (or a top-level `code` or `type`).
fn document_kind(message: &str) -> Option<&'static str> {
    let trimmed = message.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let doc: Value = serde_json::from_str(trimmed).ok()?;
    let inner = doc.get("error").filter(|e| e.is_object()).unwrap_or(&doc);
    let field = |key: &str| inner.get(key).and_then(Value::as_str);
    let by_code = match field("code") {
        Some("insufficient_quota" | "usage_limit_reached" | "usage_limit_exceeded") => {
            Some("provider_usage_limit")
        }
        Some("context_length_exceeded" | "context_window_exceeded") => {
            Some("provider_context_exhausted")
        }
        Some("invalid_api_key") => Some("provider_auth"),
        Some("model_not_found") => Some("provider_model_unavailable"),
        Some("rate_limit_exceeded" | "server_overloaded") => Some("provider_capacity"),
        _ => None,
    };
    by_code.or(match field("type") {
        Some("insufficient_quota" | "usage_limit_reached") => Some("provider_usage_limit"),
        Some("authentication_error" | "permission_error") => Some("provider_auth"),
        Some("rate_limit_error" | "overloaded_error" | "server_overloaded") => {
            Some("provider_capacity")
        }
        _ => None,
    })
}

/// Whether lowercased text says a model does not exist in the form
/// ``model `name` does not exist`` (or with `'` or `"` around the name): the
/// word "model" directly before the quoted name, and "does not exist" directly
/// after it.
fn names_missing_model(lower: &str) -> bool {
    lower.match_indices("model ").any(|(at, word)| {
        // "model" must start a word, so "submodel" does not count.
        let starts_word = lower[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        if !starts_word {
            return false;
        }
        let rest = &lower[at + word.len()..];
        let Some(quote) = rest
            .chars()
            .next()
            .filter(|c| matches!(c, '`' | '\'' | '"'))
        else {
            return false;
        };
        let name = &rest[1..];
        name.find(quote)
            .is_some_and(|end| end > 0 && name[end + 1..].starts_with(" does not exist"))
    })
}

/// The start of Codex's content-policy refusal, as codex-cli 0.171.0 reported
/// it on 2026-11-24 in an `error` event and the `turn.failed` after it, with no
/// code: "This content was flagged for possible cybersecurity risk. If this
/// seems wrong, try rephrasing your request. ..." Only this observed category
/// is matched. Another wording, such as "flagged for possible review", keeps
/// `provider_error` (or the kind its other words name) until it is observed.
const CONTENT_POLICY_TEXT: &str = "this content was flagged for possible cybersecurity risk";

/// The error kind of a message by its words. Codex 0.170.0 gives no codes in
/// `codex exec --json`, only text.
fn text_kind(message: &str) -> Option<&'static str> {
    let lower = message.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    // A content-policy refusal first: its text starts with the observed
    // phrase, and the rest of it may mention access or limits. Then the same order as
    // the structured kinds: usage limit, auth, model, context, capacity (Codex
    // text names no billing failure).
    if lower.trim_start().starts_with(CONTENT_POLICY_TEXT) {
        Some("provider_content_policy")
    } else if has(&[
        "usage limit",
        "exceeded your current quota",
        "insufficient_quota",
    ]) {
        Some("provider_usage_limit")
    } else if has(&["unexpected status 401", "unexpected status 403"]) {
        Some("provider_auth")
    } else if has(&["model is not supported", "model_not_found"]) || names_missing_model(&lower) {
        Some("provider_model_unavailable")
    } else if has(&["context window"]) {
        Some("provider_context_exhausted")
    } else if has(&[
        "unexpected status 429",
        "unexpected status 503",
        "unexpected status 529",
        "rate limit",
        "overloaded",
        "at capacity",
    ]) {
        Some("provider_capacity")
    } else {
        None
    }
}

/// The name in a `codex_error_info` on the event or its `error`, when it is a
/// string or an object with one key.
fn info_name(event: &Map<String, Value>) -> Option<&str> {
    let info = event
        .get("codex_error_info")
        .or_else(|| event.get("error").and_then(|e| e.get("codex_error_info")))?;
    match info {
        Value::String(s) if !s.is_empty() => Some(s.as_str()),
        Value::Object(map) if map.len() == 1 => map.keys().next().map(String::as_str),
        _ => None,
    }
}

/// The error kind of a failed turn or an error event: a structured
/// `codex_error_info` on the event or its `error` first, then a message that
/// is a JSON error document, then the message's words. `None` keeps
/// `provider_error`.
fn failure_kind(event: &Map<String, Value>, message: &str) -> Option<&'static str> {
    info_name(event)
        .and_then(info_kind)
        .or_else(|| document_kind(message))
        .or_else(|| text_kind(message))
}

/// Reduce the output stream to a [`Parsed`].
pub(super) fn parse(bytes: &[u8]) -> Parsed {
    let mut parsed = Parsed::default();
    let mut declined_id: Option<String> = None;
    let stats = for_each_event(bytes, |event| {
        let kind = str_field(event, "type").unwrap_or_default();
        // The first terminal event decides; later events only count for the
        // session ids the stream reported.
        if parsed.terminal != Terminal::None {
            if matches!(kind, "turn.completed" | "turn.failed") {
                parsed.note_second_terminal();
            }
            if kind == "thread.started"
                && let Some(id) = str_field(event, "thread_id")
            {
                parsed.saw_late_session(id);
            }
            return;
        }
        let item = event.get("item");
        let item_id = item.and_then(|i| i.get("id")).and_then(Value::as_str);
        // Any item other than the declined command itself means the run went on.
        if matches!(kind, "item.started" | "item.completed")
            && item_id.is_none_or(|id| declined_id.as_deref() != Some(id))
        {
            parsed.stopped_by_denial = false;
        }
        let item_type = item.and_then(|i| i.get("type")).and_then(Value::as_str);
        let status = item.and_then(|i| i.get("status")).and_then(Value::as_str);
        match kind {
            "thread.started" => {
                if let Some(id) = str_field(event, "thread_id") {
                    parsed.saw_session(id);
                }
            }
            "item.completed" if item_type == Some("agent_message") => {
                let text = item.and_then(|i| i.get("text")).and_then(Value::as_str);
                let commentary =
                    item.and_then(|i| i.get("phase")).and_then(Value::as_str) == Some("commentary");
                // Commentary is progress, never the answer.
                if let Some(text) = text.filter(|t| !t.is_empty() && !commentary) {
                    parsed.last_text = Some(text.to_string());
                    parsed.final_text = Some(text.to_string());
                    parsed.stopped_by_denial = false;
                }
            }
            "item.completed" if item_type == Some("command_execution") => {
                if status == Some("declined") {
                    let command = item.and_then(|i| i.get("command")).and_then(Value::as_str);
                    parsed.deny("command", command, None);
                    parsed.stopped_by_denial = true;
                    declined_id = item_id.map(str::to_string);
                }
            }
            "turn.completed" => {
                parsed.terminal = Terminal::Success;
                usages(event, &mut parsed);
            }
            "turn.failed" => {
                let text = event
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty());
                let message = match (text, info_name(event)) {
                    (Some(text), _) => text.to_string(),
                    (None, Some(name)) => format!("Codex reported {name}"),
                    (None, None) => "Codex turn failed".to_string(),
                };
                parsed.terminal = Terminal::Failure {
                    code: failure_kind(event, &message),
                    message,
                };
                usages(event, &mut parsed);
            }
            "error" => {
                // An event with only a structured `codex_error_info` counts
                // too, with a message naming it.
                let message = match (str_field(event, "message"), info_name(event)) {
                    (Some(text), _) => Some(text.to_string()),
                    (None, Some(name)) => Some(format!("Codex reported {name}")),
                    (None, None) => None,
                };
                if let Some(message) = message {
                    parsed.error_code = failure_kind(event, &message);
                    parsed.saw_error(&message);
                    parsed.stopped_by_denial = false;
                }
            }
            _ => {}
        }
    });
    parsed.stats = stats;
    parsed
}

/// Arguments for the MCP discovery probe, which lists the servers Codex knows.
pub(super) fn discovery_args() -> Vec<String> {
    let mut args: Vec<String> = DISABLED_FEATURES
        .iter()
        .flat_map(|f| ["--disable".to_string(), (*f).to_string()])
        .collect();
    args.extend(["mcp", "list", "--json"].map(String::from));
    args
}

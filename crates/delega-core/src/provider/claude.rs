//! Claude Code: launch arguments and output stream.
//!
//! Commands run with `-p --output-format stream-json`, the prompt on standard
//! input. The permission mapping is explained in `docs/providers.md`.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::contract::permissions::Mode;
use crate::contract::request::Effort;
use crate::contract::result::Usage;

use super::hook::{GRANTS_ENV, HookCommand};
use super::launch::{Built, Elide, Inputs};
use super::limits::{
    DENIAL_REASON_CHARS, HOOK_TIMEOUT_SECS, MAX_REASONS, PROVIDER_MESSAGE_CHARS,
    SETTINGS_ARGUMENT_LIMIT,
};
use super::refusal::{Refusal, clip};
use super::stream::{Parsed, Terminal, count, for_each_event, str_field};

/// The built-in tools `full` grants. Anything else a Claude Code install offers
/// (sub-agents, MCP tools, background tasks) stays off.
const BUILTINS: [&str; 9] = [
    "Read",
    "Glob",
    "Grep",
    "Bash",
    "Edit",
    "Write",
    "NotebookEdit",
    "WebFetch",
    "WebSearch",
];

fn effort(e: Effort) -> &'static str {
    match e {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
    }
}

/// Characters other than letters and digits that may appear in a path written
/// into a rule. Everything else is refused: a rule is comma-separated text with
/// parentheses and glob syntax, and a path that could end or split a rule, or
/// read as a pattern, must never reach it.
const PATH_PUNCTUATION: &str = " /._-+@=:%~";

/// Code point ranges whose characters fold to punctuation or digits under
/// compatibility normalisation (enclosed and parenthesised alphanumerics,
/// number and Roman numeral forms, super and subscripts, letterlike symbols,
/// fullwidth and halfwidth forms, enclosed and compatibility CJK).
const FOLDING_BLOCKS: [(u32, u32); 8] = [
    (0x2070, 0x209F),
    (0x2100, 0x218F),
    (0x2460, 0x24FF),
    (0x3200, 0x33FF),
    (0xFF00, 0xFFEF),
    (0x1F100, 0x1F1FF),
    (0x1F200, 0x1F2FF),
    (0xFE30, 0xFE6F),
];

fn in_folding_block(c: char) -> bool {
    let n = c as u32;
    FOLDING_BLOCKS
        .iter()
        .any(|(lo, hi)| (*lo..=*hi).contains(&n))
}

/// Whether a character may appear in a path written into a rule: an ASCII
/// letter or digit, one of [`PATH_PUNCTUATION`], or a letter of any script that
/// is not in a block that folds to punctuation. Non-ASCII digits are refused.
fn path_char_ok(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || PATH_PUNCTUATION.contains(c)
        || (!c.is_ascii() && c.is_alphabetic() && !in_folding_block(c))
}

/// Tool names, and the prefix of any tool name pattern, that a reader splitting
/// a rule list on whitespace would take as a rule of its own.
const TOOL_NAMES: [&str; 20] = [
    "read",
    "glob",
    "grep",
    "bash",
    "edit",
    "write",
    "notebookedit",
    "webfetch",
    "websearch",
    "task",
    "agent",
    "todowrite",
    "multiedit",
    "notebookread",
    "ls",
    "bashoutput",
    "killshell",
    "slashcommand",
    "exitplanmode",
    "mcp__",
];

/// Whether a word, as a whitespace-splitting reader sees it, is a tool name or
/// the start of a tool rule.
fn is_tool_word(word: &str) -> bool {
    let lower = word.to_lowercase();
    TOOL_NAMES.iter().any(|name| {
        lower == *name
            || lower.starts_with(&format!("{name}("))
            || (name.ends_with("__") && lower.starts_with(name))
    })
}

/// Whether text written into a rule has, after its first word, a word that a
/// reader splitting on whitespace would take for a tool or a tool rule. The
/// first word is attached to the rule's own `Tool(` prefix and cannot stand
/// alone. Every Unicode whitespace character counts as a delimiter.
fn has_tool_word(text: &str) -> bool {
    text.split(char::is_whitespace).skip(1).any(is_tool_word)
}

/// Whether a path can be written into a rule as it is.
pub(super) fn path_is_rule_safe(text: &str) -> bool {
    text.chars().all(path_char_ok) && !has_tool_word(text)
}

/// A rule path for an absolute directory: `//abs/dir/**`.
fn rule_path(dir: &Path, kind: &str) -> Result<String, Refusal> {
    let unusable = |why: &str| {
        Refusal::new(
            kind,
            format!(
                "{} cannot be written as a Claude Code rule: {why}. Letters, digits and \
                 `{PATH_PUNCTUATION}` are allowed in the path, and no word after the first \
                 may be a tool name; use a path without other characters",
                dir.display()
            ),
        )
    };
    let text = dir
        .to_str()
        .ok_or_else(|| unusable("it is not valid UTF-8"))?;
    if !path_is_rule_safe(text) {
        return Err(unusable(
            "it contains a character that could end or split a rule or read as a pattern, or a \
             word that reads as a tool name",
        ));
    }
    let inner = text.trim_matches('/');
    Ok(if inner.is_empty() {
        "//**".to_string()
    } else {
        format!("//{inner}/**")
    })
}

/// The `--settings` value: the command guard hook, or hooks switched off, and
/// the user's deny rules when there are any.
///
/// Deny rules are written only as elements of the `permissions.deny` array of
/// this JSON document, which `serde_json` escapes. Text in a rule cannot end its
/// string or the array, so it cannot add a key such as `permissions.allow`, and
/// the `--allowedTools` value is built from delega's own data alone. A string in
/// a deny list is a deny pattern whatever it contains, so a rule that is not
/// well formed matches less and never allows anything.
fn settings_json(guard: Option<&HookCommand>, deny: &[String]) -> Value {
    crate::canonical::sorted(settings_value(guard, deny))
}

fn settings_value(guard: Option<&HookCommand>, deny: &[String]) -> Value {
    let mut settings = match guard {
        Some(hook) => json!({
            "disableAllHooks": false,
            "hooks": { "PreToolUse": [ { "matcher": "Bash", "hooks": [ {
                "type": "command",
                "command": hook.program.to_string_lossy(),
                "args": hook.args,
                "timeout": HOOK_TIMEOUT_SECS,
                "onFailure": "block"
            } ] } ] }
        }),
        None => json!({ "disableAllHooks": true }),
    };
    if !deny.is_empty() {
        settings["permissions"] = json!({ "deny": deny });
    }
    settings
}

/// Characters refused in a command that becomes a rule: parentheses and commas
/// end or split a rule, `*` is a wildcard, a backslash escapes, and control
/// characters (line breaks included) are never part of an exact command.
const COMMAND_REFUSED: [char; 5] = ['(', ')', ',', '*', '\\'];

/// A command that cannot be written as an exact Claude Code rule.
fn check_command(index: usize, command: &str) -> Result<(), Refusal> {
    let bad_char = |c: char| {
        c.is_control()
            || COMMAND_REFUSED.contains(&c)
            || c == '\u{2028}'
            || c == '\u{2029}'
            || (0xFF00..=0xFFEF).contains(&(c as u32))
    };
    if command.chars().any(bad_char) || command.trim() != command || has_tool_word(command) {
        return Err(Refusal::new(
            "unsupported_command",
            format!(
                "permissions.commands[{index}] contains a parenthesis, comma, asterisk, \
                 backslash, control, line-separator or fullwidth character, has leading or \
                 trailing whitespace, or has a word after the first that reads as a tool name; \
                 Claude Code rules treat these as syntax, so an exact command cannot be written \
                 for it"
            ),
        ));
    }
    Ok(())
}

struct Grants {
    tools: Vec<&'static str>,
    allowed: Vec<String>,
    guard: Option<HookCommand>,
    commands: Vec<String>,
    notes: Vec<String>,
    unenforceable: Vec<String>,
}

fn grants(inputs: &Inputs<'_>) -> Result<Grants, Refusal> {
    let scope = inputs.scope;
    let mut notes = Vec::new();
    if scope.mode == Mode::Full {
        notes.push(
            "full grants the nine built-in tools without a scope; tools beyond them (sub-agents, \
             MCP tools, background tasks) stay off"
                .to_string(),
        );
        return Ok(Grants {
            tools: BUILTINS.to_vec(),
            allowed: BUILTINS.iter().map(ToString::to_string).collect(),
            guard: None,
            commands: Vec::new(),
            notes,
            unenforceable: Vec::new(),
        });
    }

    let mut tools = vec!["Read", "Glob", "Grep"];
    let mut allowed = vec![format!("Read({})", rule_path(&scope.cwd, "cwd_unusable")?)];
    for dir in &scope.directories {
        allowed.push(format!("Read({})", rule_path(dir, "directory_unusable")?));
    }
    // Glob and Grep stay in --tools but are not allow rules: Claude Code then
    // applies the scoped Read rules above to them.
    if scope.mode == Mode::Write {
        tools.extend(["Edit", "Write"]);
        allowed.push(format!("Edit({})", rule_path(&scope.cwd, "cwd_unusable")?));
        allowed.push(format!("Write({})", rule_path(&scope.cwd, "cwd_unusable")?));
        notes.push("NotebookEdit is not granted in write mode".to_string());
    }

    let mut guard = None;
    let mut commands = Vec::new();
    let mut unenforceable = Vec::new();
    if !scope.commands.is_empty() {
        for (i, command) in scope.commands.iter().enumerate() {
            check_command(i, command)?;
        }
        match inputs.hook {
            Some(hook) => {
                tools.push("Bash");
                for command in &scope.commands {
                    allowed.push(format!("Bash({command})"));
                }
                guard = Some(hook.clone());
                commands = scope.commands.clone();
            }
            None => {
                notes.push(
                    "permissions.commands is not granted: no command guard is configured, and \
                     Claude Code's own rules would match more than the exact command"
                        .to_string(),
                );
                unenforceable.push(
                    "permissions.commands cannot be granted exactly because no command guard is \
                     configured"
                        .to_string(),
                );
            }
        }
    }
    Ok(Grants {
        tools,
        allowed,
        guard,
        commands,
        notes,
        unenforceable,
    })
}

/// Build the launch for a request.
/// Variables delega sets for every Claude worker, which a caller cannot pass or
/// set (`forbidden_env_name`). Background tasks would outlive the run. The two
/// memory switches, both documented by Claude Code, stop it loading the user's
/// `CLAUDE.md` files and auto memory (`~/.claude/projects/<project>/memory/`)
/// into the worker's context and turn the auto-memory feature off:
/// `--setting-sources ""` keeps `CLAUDE.md` out but not auto memory (#106).
pub(super) const OWNED_ENV: [(&str, &str); 3] = [
    ("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1"),
    ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1"),
    ("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1"),
];

pub(super) fn build(inputs: &Inputs<'_>) -> Result<Built, Refusal> {
    let request = inputs.request;
    let scope = inputs.scope;
    let g = grants(inputs)?;
    let settings = settings_json(g.guard.as_ref(), &inputs.deny.rules).to_string();
    if settings.len() > SETTINGS_ARGUMENT_LIMIT {
        let kind = if inputs.deny.rules.is_empty() {
            "unsupported_command"
        } else {
            super::deny::OVER_LIMIT
        };
        return Err(Refusal::new(
            kind,
            format!(
                "the generated Claude Code settings are {} bytes, over the {SETTINGS_ARGUMENT_LIMIT} \
                 byte limit; a restriction is never dropped to make it fit",
                settings.len()
            ),
        ));
    }

    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        "dontAsk",
        "--permission-prompts",
        "none",
        "--strict-mcp-config",
        "--mcp-config",
        "{\"mcpServers\":{}}",
        "--setting-sources",
        "",
        "--settings",
    ]
    .map(String::from)
    .to_vec();
    args.push(settings);
    let cwd = scope.cwd.to_string_lossy().into_owned();
    args.extend(["--add-dir".to_string(), cwd]);
    for dir in &scope.directories {
        args.extend(["--add-dir".to_string(), dir.to_string_lossy().into_owned()]);
    }
    args.extend(["--disable-slash-commands", "--no-chrome"].map(String::from));
    args.extend(["--tools".to_string(), g.tools.join(",")]);
    args.extend(["--allowedTools".to_string(), g.allowed.join(",")]);
    if let Some(id) = &request.session.resume {
        args.extend(["--resume".to_string(), id.clone()]);
    }
    if request.session.ephemeral {
        args.push("--no-session-persistence".to_string());
    }
    if let Some(model) = &request.model {
        args.extend(["--model".to_string(), model.clone()]);
    }
    if let Some(e) = request.effort {
        args.extend(["--effort".to_string(), effort(e).to_string()]);
    }
    args.push("--append-system-prompt".to_string());
    let system_index = args.len();
    args.push(inputs.system.to_string());
    args.extend(inputs.native.iter().cloned());

    let mut env: Vec<(String, String)> = OWNED_ENV
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    if g.guard.is_some() {
        env.push((
            GRANTS_ENV.to_string(),
            Value::from(g.commands.clone()).to_string(),
        ));
    }

    let mut notes = g.notes;
    notes.push(
        "the user's and the project's Claude Code settings other than their deny rules were not \
         applied: --setting-sources is empty, so their hooks, env, apiKeyHelper, plugins, skills, \
         MCP servers and instructions are not loaded"
            .to_string(),
    );
    notes.extend(inputs.deny.notes());
    notes.push(
        "Claude Code loads no CLAUDE.md file and no auto memory into the worker's context by \
         itself: CLAUDE_CODE_DISABLE_CLAUDE_MDS=1 stops it loading the user's, the project's \
         and reference directories' CLAUDE.md files and the auto-memory index, and \
         CLAUDE_CODE_DISABLE_AUTO_MEMORY=1 turns its auto-memory feature off; the worker can \
         still read such files with its tools, as the delegated preamble asks it to for \
         repository instructions"
            .to_string(),
    );
    let mut mapping = vec![
        "permission-mode=dontAsk".to_string(),
        format!("tools={}", g.tools.join(",")),
        format!("allowedTools={}", g.allowed.join(",")),
        format!("denyRules={}", inputs.deny.rules.len()),
        format!(
            "hooks={}",
            if g.guard.is_some() {
                "exact-command-guard"
            } else {
                "disabled"
            }
        ),
    ];
    if request.session.ephemeral {
        mapping.push("session=ephemeral".to_string());
    }
    Ok(Built {
        args,
        env,
        notes,
        unenforceable: g.unenforceable,
        settings: mapping,
        commands: g.commands,
        elide: vec![Elide {
            index: system_index,
            prefix: "",
        }],
        needs_hook_version: g.guard.is_some(),
        mcp_index: None,
    })
}

fn assistant_text(event: &Map<String, Value>) -> Option<String> {
    let blocks = event.get("message")?.get("content")?.as_array()?;
    let text: String = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();
    (!text.is_empty()).then_some(text)
}

fn denial_target(item: &Value) -> Option<String> {
    let input = item.get("tool_input")?;
    let keys: &[&str] = if item.get("tool_name").and_then(Value::as_str) == Some("Bash") {
        &["command"]
    } else {
        &["file_path", "path", "notebook_path", "pattern", "url"]
    };
    keys.iter()
        .find_map(|k| input.get(k).and_then(Value::as_str))
        .map(str::to_string)
}

fn usage(event: &Map<String, Value>) -> Option<Usage> {
    let counts = event.get("usage");
    let cost = event.get("total_cost_usd").and_then(Value::as_f64);
    let usage = Usage {
        // Input tokens that were sent, including those written to the cache.
        input_tokens: counts.and_then(|u| {
            let plain = count(u, "input_tokens");
            let created = count(u, "cache_creation_input_tokens");
            match (plain, created) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
            }
        }),
        output_tokens: counts.and_then(|u| count(u, "output_tokens")),
        cached_tokens: counts.and_then(|u| count(u, "cache_read_input_tokens")),
        reasoning_tokens: None,
        cost_usd: cost.filter(|c| c.is_finite() && *c >= 0.0),
    };
    let empty = usage.input_tokens.is_none()
        && usage.output_tokens.is_none()
        && usage.cached_tokens.is_none()
        && usage.cost_usd.is_none();
    (!empty).then_some(usage)
}

fn failure_message(event: &Map<String, Value>) -> String {
    let errors: Vec<&str> = event
        .get("errors")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let text = if !errors.is_empty() {
        errors.join("; ")
    } else if let Some(result) = str_field(event, "result") {
        // An API failure (an unknown model, a bad key, a usage limit) carries
        // its reason in `result` and has no `errors`.
        result.to_string()
    } else {
        match str_field(event, "subtype") {
            Some("success") | None => "Claude Code reported an error".to_string(),
            Some(subtype) => format!("Claude Code ended with {subtype}"),
        }
    };
    clip(&text, PROVIDER_MESSAGE_CHARS)
}

/// The API error fields of a root assistant message marked
/// `is_api_error_message`, or of a failed result. Only these short fields are
/// kept, so the state stays bounded however long the stream is.
#[derive(Debug, Clone, Default)]
struct ApiError {
    /// The assistant message's `error`, such as `model_not_found`.
    error: Option<String>,
    /// `api_error`, such as `usage_limit_reached`.
    api_error: Option<String>,
    /// `api_error_type`, such as `authentication_error`.
    kind: Option<String>,
    /// `api_error_status`, the HTTP status.
    status: Option<u64>,
}

/// The longest API error field kept; the known values are much shorter.
const API_FIELD_CHARS: usize = 64;

fn api_error(event: &Map<String, Value>) -> ApiError {
    let field = |key: &str| str_field(event, key).map(|v| clip(v, API_FIELD_CHARS));
    ApiError {
        error: field("error"),
        api_error: field("api_error"),
        kind: field("api_error_type"),
        status: event.get("api_error_status").and_then(Value::as_u64),
    }
}

/// Whether a root `rate_limit_event` says the account is at a usage limit:
/// its status is `rejected` and overage does not cover the request.
fn rate_limit_rejected(event: &Map<String, Value>) -> bool {
    let Some(info) = event.get("rate_limit_info") else {
        return false;
    };
    let text = |key: &str| info.get(key).and_then(Value::as_str);
    let overage = info.get("isUsingOverage").and_then(Value::as_bool) == Some(true)
        || matches!(text("overageStatus"), Some("allowed" | "allowed_warning"));
    text("status") == Some("rejected") && !overage
}

/// Assistant `error` values that mean the account or its credentials cannot
/// be used.
const AUTH_ERRORS: [&str; 5] = [
    "authentication_failed",
    "oauth_org_not_allowed",
    "account_on_hold",
    "verification_required",
    "cloud_credential_error",
];

/// The error kind of a failed result that names no turn or budget limit, from
/// the result, the last root API error message and the latest root
/// `rate_limit_event`. `None` keeps `provider_error`.
fn failure_kind(
    result: &Map<String, Value>,
    message: Option<&ApiError>,
    usage_limited: bool,
) -> Option<&'static str> {
    let own = api_error(result);
    let sources: Vec<&ApiError> = std::iter::once(&own).chain(message).collect();
    let error = message.and_then(|m| m.error.as_deref());
    let any = |pick: fn(&ApiError) -> Option<&str>, values: &[&str]| {
        sources
            .iter()
            .any(|s| pick(s).is_some_and(|v| values.contains(&v)))
    };
    let kind_is = |values: &[&str]| any(|s| s.kind.as_deref(), values);
    let status_is = |values: &[u64]| {
        sources
            .iter()
            .any(|s| s.status.is_some_and(|v| values.contains(&v)))
    };
    let error_is = |values: &[&str]| error.is_some_and(|e| values.contains(&e));
    let text = str_field(result, "result").unwrap_or_default();

    if usage_limited || any(|s| s.api_error.as_deref(), &["usage_limit_reached"]) {
        Some("provider_usage_limit")
    } else if error_is(&["billing_error"]) || kind_is(&["billing_error"]) {
        Some("provider_billing")
    } else if error_is(&AUTH_ERRORS)
        || kind_is(&["authentication_error", "permission_error"])
        || status_is(&[401, 403])
    {
        Some("provider_auth")
    } else if error_is(&["model_not_found"]) || (kind_is(&["not_found_error"]) && status_is(&[404]))
    {
        Some("provider_model_unavailable")
    } else if text.starts_with("Prompt is too long") {
        Some("provider_context_exhausted")
    } else if error_is(&["rate_limit", "overloaded"])
        || kind_is(&["rate_limit_error", "overloaded_error"])
        || status_is(&[429, 503, 529])
        || ["API Error: 429", "API Error: 503", "API Error: 529"]
            .iter()
            .any(|p| text.starts_with(p))
    {
        Some("provider_capacity")
    } else {
        None
    }
}

/// Whether an error text from the failed result is about a denial: it names a
/// denied tool or speaks of permission or denial. Anything else, such as a lost
/// connection, is another cause.
fn mentions_denial(error: &str, denied_tools: &[String]) -> bool {
    let lower = error.to_lowercase();
    [
        "permission",
        "denied",
        "denial",
        "not allowed",
        "not granted",
    ]
    .iter()
    .any(|w| lower.contains(w))
        || denied_tools
            .iter()
            .any(|t| !t.is_empty() && lower.contains(t.as_str()))
}

/// The first sentence of a tool error, as a short reason.
fn error_reason(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect(),
        _ => return None,
    };
    let sentence_end = text.find(". ").map_or(text.len(), |i| i + 1);
    let line_end = text.find('\n').unwrap_or(text.len());
    let reason = text[..sentence_end.min(line_end)].trim();
    (!reason.is_empty()).then(|| clip(reason, DENIAL_REASON_CHARS))
}

/// Reduce the output stream to a [`Parsed`].
pub(super) fn parse(bytes: &[u8]) -> Parsed {
    let mut parsed = Parsed::default();
    let mut reasons: HashMap<String, String> = HashMap::new();
    // Event positions: when each tool call's result arrived, and when the last
    // piece of work (a worker message or a tool result) happened.
    let mut seq = 0usize;
    let mut result_at: HashMap<String, usize> = HashMap::new();
    let mut last_work = 0usize;
    // The last root assistant message, when it was an API error message, and
    // whether the latest root rate limit event said the usage limit was hit.
    let mut api_message: Option<ApiError> = None;
    let mut usage_limited = false;
    let stats = for_each_event(bytes, |event| {
        seq += 1;
        let kind = str_field(event, "type");
        // A sub-agent's events are not the worker's: neither their text nor
        // their session id is used.
        let subagent = event
            .get("parent_tool_use_id")
            .is_some_and(|v| !v.is_null());
        let session = str_field(event, "session_id").filter(|_| !subagent);
        // The first terminal event decides; later events only count for the
        // session ids the stream reported.
        if parsed.terminal != Terminal::None {
            if kind == Some("result") {
                parsed.note_second_terminal();
            }
            if let Some(id) = session {
                parsed.saw_late_session(id);
            }
            return;
        }
        if let Some(id) = session {
            parsed.saw_session(id);
        }
        match kind {
            Some("assistant") => {
                // A sub-agent's messages are not the worker's answer.
                if subagent {
                    return;
                }
                if let Some(text) = assistant_text(event) {
                    parsed.last_text = Some(text);
                }
                api_message = (event.get("is_api_error_message").and_then(Value::as_bool)
                    == Some(true))
                .then(|| api_error(event));
                last_work = seq;
            }
            Some("user") => {
                last_work = seq;
                let blocks = event
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(Value::as_array);
                for block in blocks.into_iter().flatten() {
                    let is_error = block.get("is_error").and_then(Value::as_bool) == Some(true);
                    let id = block.get("tool_use_id").and_then(Value::as_str);
                    if let Some(id) = id {
                        result_at.insert(id.to_string(), seq);
                    }
                    if let (true, Some(id), Some(reason)) =
                        (is_error, id, block.get("content").and_then(error_reason))
                        && reasons.len() < MAX_REASONS
                    {
                        reasons.insert(id.to_string(), reason);
                    }
                }
            }
            Some("error") => {
                let message =
                    str_field(event, "message").unwrap_or("Claude Code reported an error");
                parsed.errors.push(clip(message, PROVIDER_MESSAGE_CHARS));
            }
            Some("system") if str_field(event, "subtype") == Some("api_retry") => {
                parsed
                    .errors
                    .push("Claude Code is retrying a failed API request".to_string());
            }
            Some("rate_limit_event") if !subagent => {
                usage_limited = rate_limit_rejected(event);
            }
            Some("result") => {
                let subtype = str_field(event, "subtype");
                let success = subtype == Some("success")
                    && event.get("is_error").and_then(Value::as_bool) != Some(true);
                let code = match subtype {
                    Some("error_max_turns") => Some("provider_turn_limit"),
                    Some("error_max_budget_usd") => Some("provider_budget_limit"),
                    _ if success => None,
                    _ => failure_kind(event, api_message.as_ref(), usage_limited),
                };
                parsed.final_text = str_field(event, "result").map(str::to_string);
                parsed.terminal = if success {
                    Terminal::Success
                } else {
                    Terminal::Failure {
                        message: failure_message(event),
                        code,
                    }
                };
                parsed.usage = usage(event);
                if let Some(items) = event.get("permission_denials").and_then(Value::as_array) {
                    for item in items {
                        let tool = item
                            .get("tool_name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown tool");
                        let reason = item
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .and_then(|id| reasons.get(id));
                        parsed.deny(
                            tool,
                            denial_target(item).as_deref(),
                            reason.map(String::as_str),
                        );
                    }
                }
                // Work after the last denied call's result means the run went on.
                let last_denied_at = event
                    .get("permission_denials")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|d| d.get("tool_use_id").and_then(Value::as_str))
                    .filter_map(|id| result_at.get(id))
                    .max()
                    .copied();
                let went_on = last_denied_at.is_some_and(|at| last_work > at);
                let tools: Vec<String> = parsed
                    .denied
                    .iter()
                    .map(|d| {
                        d.request
                            .split([':', ' '])
                            .next()
                            .unwrap_or_default()
                            .to_lowercase()
                    })
                    .collect();
                let errors_are_about_denial = event
                    .get("errors")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .all(|e| mentions_denial(e, &tools));
                parsed.stopped_by_denial = !success
                    && code.is_none()
                    && !parsed.denied.is_empty()
                    && parsed.errors.is_empty()
                    && !went_on
                    && errors_are_about_denial;
            }
            _ => {}
        }
    });
    parsed.stats = stats;
    parsed
}

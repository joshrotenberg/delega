//! Claude Code: launch arguments and output stream.
//!
//! Commands run with `-p --output-format stream-json`, the prompt on standard
//! input. The permission mapping is explained in `docs/providers.md`.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::contract::permissions::Mode;
use crate::contract::request::Effort;
use crate::contract::result::Usage;

use super::hook::{GRANTS_ENV, HookCommand};
use super::launch::{Built, Elide, Inputs};
use super::limits::{HOOK_TIMEOUT_SECS, PROVIDER_MESSAGE_CHARS, SETTINGS_ARGUMENT_LIMIT};
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

/// A rule path for an absolute directory: `//abs/dir/**`, with the characters
/// that are special in a rule escaped.
fn rule_path(dir: &Path) -> Result<String, Refusal> {
    let text = dir.to_str().ok_or_else(|| {
        Refusal::new(
            "directory_unusable",
            format!(
                "{} is not valid UTF-8 and cannot be written as a Claude Code rule",
                dir.display()
            ),
        )
    })?;
    let mut escaped = String::new();
    for ch in text.trim_matches('/').chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | '{' | '}' | '\\') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    Ok(if escaped.is_empty() {
        "//**".to_string()
    } else {
        format!("//{escaped}/**")
    })
}

/// The `--settings` value: the command guard hook, or hooks switched off.
fn settings_json(guard: Option<&HookCommand>) -> Value {
    match guard {
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
    }
}

/// A command that cannot be written as an exact Claude Code rule.
fn check_command(index: usize, command: &str) -> Result<(), Refusal> {
    if command.contains(['*', '\n', '\r']) {
        return Err(Refusal::new(
            "unsupported_command",
            format!(
                "permissions.commands[{index}] contains '*' or a line break, which Claude Code \
                 rules treat as syntax; an exact command cannot be written for it"
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
        });
    }

    let mut tools = vec!["Read", "Glob", "Grep"];
    let mut allowed = vec![format!("Read({})", rule_path(&scope.cwd)?)];
    for dir in &scope.directories {
        allowed.push(format!("Read({})", rule_path(dir)?));
    }
    allowed.push("Glob".to_string());
    allowed.push("Grep".to_string());
    notes.push(
        "Glob and Grep are granted without a path scope; whether Claude Code confines them to the \
         working directory and permissions.directories is up to the provider and is not enforced \
         by delega"
            .to_string(),
    );
    if scope.mode == Mode::Write {
        tools.extend(["Edit", "Write"]);
        allowed.push(format!("Edit({})", rule_path(&scope.cwd)?));
        allowed.push(format!("Write({})", rule_path(&scope.cwd)?));
        notes.push("NotebookEdit is not granted in write mode".to_string());
    }

    let mut guard = None;
    let mut commands = Vec::new();
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
            None => notes.push(
                "permissions.commands is not granted: no command guard is configured, and Claude \
                 Code's own rules would match more than the exact command"
                    .to_string(),
            ),
        }
    }
    Ok(Grants {
        tools,
        allowed,
        guard,
        commands,
        notes,
    })
}

/// Build the launch for a request.
pub(super) fn build(inputs: &Inputs<'_>) -> Result<Built, Refusal> {
    let request = inputs.request;
    let scope = inputs.scope;
    let g = grants(inputs)?;
    let settings = settings_json(g.guard.as_ref()).to_string();
    if settings.len() > SETTINGS_ARGUMENT_LIMIT {
        return Err(Refusal::new(
            "unsupported_command",
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

    let mut env = vec![
        (
            "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS".to_string(),
            "1".to_string(),
        ),
        (
            "CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD".to_string(),
            "1".to_string(),
        ),
    ];
    if g.guard.is_some() {
        env.push((
            GRANTS_ENV.to_string(),
            Value::from(g.commands.clone()).to_string(),
        ));
    }

    let mut notes = g.notes;
    notes.push(
        "Claude Code settings sources are not loaded (--setting-sources is empty): the user's and \
         the project's own deny rules, hooks and instructions do not apply to this worker"
            .to_string(),
    );
    let mut mapping = vec![
        "permission-mode=dontAsk".to_string(),
        format!("tools={}", g.tools.join(",")),
        format!("allowedTools={}", g.allowed.join(",")),
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
        input_tokens: counts.and_then(|u| count(u, "input_tokens")),
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
    let text = if errors.is_empty() {
        match str_field(event, "subtype") {
            Some(subtype) => format!("Claude Code ended with {subtype}"),
            None => "Claude Code reported an error".to_string(),
        }
    } else {
        errors.join("; ")
    };
    clip(&text, PROVIDER_MESSAGE_CHARS)
}

/// Reduce the output stream to a [`Parsed`].
pub(super) fn parse(bytes: &[u8]) -> Parsed {
    let mut parsed = Parsed::default();
    let stats = for_each_event(bytes, |event| {
        if let Some(id) = str_field(event, "session_id") {
            parsed.session_id = Some(id.to_string());
        }
        match str_field(event, "type") {
            Some("assistant") => {
                // A sub-agent's messages are not the worker's answer.
                if event
                    .get("parent_tool_use_id")
                    .is_some_and(|v| !v.is_null())
                {
                    return;
                }
                if let Some(text) = assistant_text(event) {
                    parsed.last_text = Some(text);
                }
            }
            Some("result") => {
                let success = str_field(event, "subtype") == Some("success")
                    && event.get("is_error").and_then(Value::as_bool) != Some(true);
                parsed.final_text = str_field(event, "result").map(str::to_string);
                parsed.terminal = if success {
                    Terminal::Success
                } else {
                    Terminal::Failure(failure_message(event))
                };
                parsed.usage = usage(event);
                parsed.denied.clear();
                parsed.denied_omitted = 0;
                if let Some(items) = event.get("permission_denials").and_then(Value::as_array) {
                    for item in items {
                        let tool = item
                            .get("tool_name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown tool");
                        parsed.deny(tool, denial_target(item).as_deref());
                    }
                }
            }
            _ => {}
        }
    });
    parsed.stats = stats;
    parsed
}

//! Codex CLI: launch arguments and output stream.
//!
//! Commands run as `codex exec --json`, the prompt on standard input. Sandbox
//! and approval policy carry the permission mapping, which is explained in
//! `docs/providers.md`.

use serde_json::{Map, Value};

use crate::contract::permissions::Mode;
use crate::contract::request::Effort;
use crate::contract::result::Usage;

use super::launch::{Built, Elide, Inputs};
use super::limits::PROVIDER_MESSAGE_CHARS;
use super::refusal::{Refusal, clip};
use super::stream::{Parsed, Terminal, count, for_each_event, str_field};

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

fn toml_string(text: &str) -> String {
    Value::from(text).to_string()
}

fn sandbox(mode: Mode) -> &'static str {
    match mode {
        Mode::Read => "read-only",
        Mode::Write => "workspace-write",
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
    if scope.mode == Mode::Write {
        settings.extend([
            "sandbox_workspace_write.network_access=false".to_string(),
            "sandbox_workspace_write.exclude_slash_tmp=true".to_string(),
            "sandbox_workspace_write.exclude_tmpdir_env_var=true".to_string(),
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
    args.push(format!(
        "developer_instructions={}",
        toml_string(inputs.system)
    ));
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
        Mode::Full => notes.push("full runs Codex with no sandbox".to_string()),
    }
    let mut unenforceable = Vec::new();
    if scope.mode != Mode::Full {
        unenforceable.push(
            "reads are not confined to the working directory and permissions.directories"
                .to_string(),
        );
        unenforceable.push(
            if scope.mode == Mode::Write {
                "commands that write inside the working directory or need no write access and no \
                 network access can run although the request allows only the commands it lists"
            } else {
                "commands that need no write access and no network access can run although the \
                 request allows only the commands it lists"
            }
            .to_string(),
        );
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

fn usage(event: &Map<String, Value>) -> Option<Usage> {
    let counts = event.get("usage")?;
    let usage = Usage {
        input_tokens: count(counts, "input_tokens"),
        output_tokens: count(counts, "output_tokens"),
        cached_tokens: count(counts, "cached_input_tokens"),
        reasoning_tokens: count(counts, "reasoning_output_tokens"),
        cost_usd: None,
    };
    let empty = usage.input_tokens.is_none()
        && usage.output_tokens.is_none()
        && usage.cached_tokens.is_none()
        && usage.reasoning_tokens.is_none();
    (!empty).then_some(usage)
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
                parsed.usage = usage(event);
            }
            "turn.failed" => {
                let message = event
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Codex turn failed");
                let lower = message.to_lowercase();
                let limit = ["usage limit", "rate limit", "quota", "context window"]
                    .iter()
                    .any(|w| lower.contains(w));
                parsed.terminal = Terminal::Failure {
                    message: clip(message, PROVIDER_MESSAGE_CHARS),
                    code: limit.then_some("provider_usage_limit"),
                };
                parsed.usage = usage(event);
            }
            "error" => {
                if let Some(message) = str_field(event, "message") {
                    parsed.errors.push(clip(message, PROVIDER_MESSAGE_CHARS));
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

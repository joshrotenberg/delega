//! The exact command guard for Claude Code.
//!
//! Claude Code's own command rules match patterns. The guard is a blocking
//! `PreToolUse` hook that allows a `Bash` call only when its command equals,
//! byte for byte, one of `permissions.commands`. It denies a call that asks for
//! a background run or for the provider's sandbox to be disabled, and it
//! denies anything it cannot parse.
//!
//! The library does not start the guard itself. The caller supplies a
//! [`HookCommand`] that runs [`run`] (the `delega` binary will do this with a
//! subcommand), and the provider layer writes it into the Claude settings. The
//! allowed commands reach the guard in the [`GRANTS_ENV`] variable as a JSON
//! array of strings.

use std::io::{Read, Write};
use std::path::PathBuf;

use serde_json::Value;

use super::limits::HOOK_INPUT_LIMIT;

/// The environment variable that carries the allowed commands to the guard.
pub const GRANTS_ENV: &str = "DELEGA_EXACT_BASH_GRANTS";
/// What the guard writes to standard error when it denies a call.
pub const DENIAL: &str = "delega: command denied by the exact command guard.\n";
/// What the guard writes to standard output when it allows a call.
pub const ALLOW_OUTPUT: &str = "{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\"}}\n";

/// A program and arguments that run the guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookCommand {
    /// The program, an absolute path.
    pub program: PathBuf,
    /// Arguments that make the program run [`run`].
    pub args: Vec<String>,
}

impl HookCommand {
    /// A hook command from a program and its arguments.
    pub fn new<I, S>(program: impl Into<PathBuf>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

/// Whether a `Bash` tool input is allowed by the grants, a JSON array of
/// complete command strings.
pub fn command_granted(grants: &Value, tool_input: &Value) -> bool {
    let Some(grants) = grants.as_array() else {
        return false;
    };
    let Some(command) = tool_input.get("command").and_then(Value::as_str) else {
        return false;
    };
    if truthy(tool_input.get("run_in_background"))
        || truthy(tool_input.get("dangerouslyDisableSandbox"))
    {
        return false;
    }
    grants.iter().any(|grant| grant.as_str() == Some(command))
}

/// Decide one hook event. `true` allows. Anything malformed denies.
pub fn decide(input: &[u8], grants: Option<&str>) -> bool {
    let Ok(event) = serde_json::from_slice::<Value>(input) else {
        return false;
    };
    let Some(grants) = grants.and_then(|g| serde_json::from_str::<Value>(g).ok()) else {
        return false;
    };
    if event.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return false;
    }
    let tool_input = event.get("tool_input").cloned().unwrap_or(Value::Null);
    command_granted(&grants, &tool_input)
}

/// Run the guard: read one hook event from `stdin`, write the decision, and
/// return the process exit status (0 allow, 2 deny). `grants` is the value of
/// [`GRANTS_ENV`], or `None` when it is unset.
pub fn run(
    stdin: &mut impl Read,
    grants: Option<&str>,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    let mut input = Vec::new();
    // Read one byte past the limit so an oversized event is denied.
    let mut limited = stdin.take(HOOK_INPUT_LIMIT as u64 + 1);
    if limited.read_to_end(&mut input).is_err() || input.len() > HOOK_INPUT_LIMIT {
        let _ = stderr.write_all(DENIAL.as_bytes());
        return 2;
    }
    if decide(&input, grants) {
        let _ = stdout.write_all(ALLOW_OUTPUT.as_bytes());
        let _ = stdout.flush();
        0
    } else {
        let _ = stderr.write_all(DENIAL.as_bytes());
        2
    }
}

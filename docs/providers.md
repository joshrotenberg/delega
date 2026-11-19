# Providers

How delega starts Claude Code and Codex CLI and what each request field turns
into. The code is `crates/delega-core/src/provider/`. The request and result
fields are defined in [api.md](api.md).

The provider behaviour is ported from an earlier project's runner (source commit `8fa4645`, named in the commit messages). This document
describes behaviour in its own terms, and states where it differs on purpose.

## Entry points

| Function | Does |
| --- | --- |
| `provider::run(request, options)` | Validates, prepares, starts the provider, waits, and returns one `RunResult`. |
| `provider::start(request, options)` | The same, returning a `RunHandle` with `interrupt()`, `interrupter()`, `is_finished()` and `wait()`. Dropping the handle interrupts the run. |
| `provider::dry_run(request, options)` | Validates and reports the executable, arguments, environment variable names, working directory, permission mapping and notes. Starts nothing, not even a version probe. |

`ProviderOptions` carries what is not part of the request: `executable` (an
absolute path), `hook` (see [Claude commands](#commands-and-directories)),
`env_pass`, `env_set`, `parent_env`, `run_id`, `worker`, `max_output_bytes`,
`stop_grace`, `progress` and `progress_text`. Its `Debug` output lists environment variable names only.

Numeric limits are in `provider/limits.rs`.

## Finding the executable

In order:

1. `ProviderOptions::executable`.
2. `DELEGA_CLAUDE_BIN` or `DELEGA_CODEX_BIN` in the parent environment.
3. `claude` or `codex` on the absolute entries of `PATH`. Relative and empty
   entries are skipped, so a program in the working directory is never picked up.

A path from 1 or 2 must be absolute and an executable file. Otherwise the run is
`refused` with kind `provider_not_found`, and the message names the variable to
set. The path used is `provenance.provider_executable`.

## Run sequence

1. Validate the request (`Request::validate`), the `native_args`, and the
   `answer_schema` (it must compile). Resolve `cwd` and `permissions.directories`
   (each must exist). Look up the executable. Any failure is `refused` before start.
2. Run `<executable> --version` (5 s). The first line is
   `provenance.provider_version`. For Claude Code with `permissions.commands`
   granted, a version below 2.1.325 or an unreadable one is `refused` with kind
   `unsupported_provider_version`.
3. Codex only: run `<executable> --disable ... mcp list --json` (10 s) and build
   an override that disables every server it lists. A failure, invalid JSON or a
   transport that cannot be disabled is `failed` with kind
   `provider_preparation_failed`, and the run does not start.
4. Start the provider with the prompt on standard input. The time limit is
   `timeout_secs` minus the time steps 2 and 3 took. If it is used up, the run is
   `timed_out` without starting.

The probes run in the working directory with the same environment as the run. A
probe's cleanup verdict is merged into the result's `cleanup`.

## Progress events

`ProviderOptions::progress` takes a `ProgressSink`, a function the provider
layer calls with a `ProgressEvent` from the task that reads the provider's
output, so it must return at once. The events are `Started` (the provider
process is about to start), `Provider { kind, summary }` (one per output line,
as it is read), `Denied { request }` (one per entry of `permissions.denied`,
after the provider has ended) and `Finished { outcome }` (always last, also for
a request refused before start). `kind` is the provider's own event type. The
summary is empty or short: for an agent message its length in characters and no
text; for a tool call, the tool name; for a Codex item, its type and status and
no command; for an error event, at most 80 characters of its text with
credentials removed (free text the provider chose to write). A `Denied` request
is the text `permissions.denied` lists, so it can name a command or a path. The
probes (`--version`, `mcp list`) report
nothing. With `ProviderOptions::progress_text` set, which is off by default, each
agent message is also sent as `Message { text }` right after the `Provider` event
that gives its length: the text blocks of a Claude Code assistant event and the
completed Codex agent message items. The text has credentials removed and is cut
to 4000 characters (`PROGRESS_TEXT_CHARS`). `delega run` leaves the option off;
the supervisor sets it to show a run's partial answer. The process layer's `ProcessSpec::stdout_tap` delivers the output chunks
through a bounded queue to a thread of its own: a slow tap loses chunks (counted
in the report's warnings) and a tap that panics gets no more, and neither delays
the run, its time limit or its capture. After the run the owner waits at most
250 ms for the tap to catch up; events a left-behind tap produces after
`Finished` are discarded, so `Finished` stays last. A `Dropped { chunks }` event,
sent once before `Finished` when the count is not zero, says how many output
chunks the tap never received, and the result's `provider_notes` says so too.

## Outcomes

| Outcome | When |
| --- | --- |
| `completed` | The provider reported success, exited with code 0, and gave answer text. With `answer_schema`, the text parsed as JSON. A denial that did not stop the run is listed in `permissions.denied`. |
| `failed` | Provider error event or result (`provider_error`, or `provider_turn_limit` or `provider_budget_limit` when Claude Code names a limit), non-zero exit (`provider_exit`, with a bounded, redacted excerpt of standard error, see [Text from the provider](#text-from-the-provider)), success followed by a non-zero exit, no terminal event and a clean exit (`no_final_message`), output that was not events (`malformed_output`), success with no answer text, `answer_not_json`, a failed preparation step, a failure to start. |
| `timed_out` | The time limit passed. `text` holds any partial answer. |
| `interrupted` | `interrupt()` was called, or the handle or future was dropped. |
| `refused` before start | See steps 1 and 2 above. `timing.started_at` is absent. |
| `refused` after start | The run stopped because of a denial, by the rule below. Kind `permission_denied`. |

**When a run is `refused` after start.** Only when all of these hold: the
provider's first terminal event is a failure; that failure is not a limit the
provider names (`provider_turn_limit`, `provider_budget_limit`,
`provider_usage_limit`); at least one request was denied; and nothing reported an
error separately and nothing went on after the last denial.

- Claude Code: the denials arrive in the failed result event. The run counts as
  stopped by the denial when no `error` event and no `api_retry` event was seen;
  when no assistant message or tool result came after the result of the last
  denied call; and when every entry in the result's `errors` names a denied tool
  or speaks of permission or denial. An entry such as "API connection lost"
  makes the run `failed`.
- Codex: the last item before the failure must be the `declined` command. Any
  later item (an agent message, another command, a `file_change`) or an `error`
  event ends the attribution. A `turn.failed` whose message speaks of a usage
  limit, rate limit, quota or context window has kind `provider_usage_limit`,
  and so is `failed`.

Every other failure with a denial is `failed` with the denial listed in
`permissions.denied`. `provider_exit_code` is in the error of every `failed` and
refused-after-start result from a process that exited. These attributions are
heuristics over events whose exact shape is unverified (see the table at the
end).

The first terminal event decides. A later terminal event is ignored and a note
says a second one was seen, so a failure followed by a success is not a
completion. Output bytes that are not valid UTF-8 are decoded with replacement
characters and a note says how many lines were affected.

### Error kinds

`invalid_request`, `invalid_answer_schema`, `native_arg_rejected`,
`cwd_unusable`, `directory_unusable`, `unsupported_command`,
`provider_not_found`, `unsupported_provider_version`, `invalid_env_name`,
`forbidden_env_name` (all refused before start);
`provider_error`, `provider_turn_limit`, `provider_budget_limit`, `provider_usage_limit`,
`provider_exit`, `no_final_message`, `malformed_output`, `answer_not_json`,
`provider_preparation_failed`, `spawn_failed`, `process_signaled`,
`wait_failed`, `internal_error` (failed); `permission_denied` (refused after
start); `timed_out`; `interrupted`.

### Text from the provider

Provider standard error and provider error messages can echo what the provider
was given. Before they go into an error message, standard error is cut to its
last 8 KiB, joined to one line, and cut again to its last 300 characters. Two
kinds of text are replaced with `[redacted]` first:

- the value of every variable passed to the provider whose name looks like a
  secret or belongs to the provider, when the value is at least 6 characters;
- strings shaped like API keys (`sk-`, `sk-ant-`), forge tokens (`ghp_`, `gho_`,
  `ghu_`, `ghs_`, `ghr_`, `github_pat_`), Slack tokens, AWS access key ids, JSON
  web tokens, `Bearer` tokens and private key blocks.

Tokens are found inside words, so `token=ghp_...`, `key: "sk-ant-..."`,
`https://user:ghp_...@host/` and JSON values are caught; `=`, `:`, `/`, `@`,
quotes and commas separate tokens. The text after `Bearer` is removed whole. The
shapes are a heuristic; a credential in another form is not caught. The
answer text itself is the provider's output and is not redacted. A denied
request records the command or path the provider asked for, as written.

A structured answer that parses but does not satisfy the schema is `completed`
with `structured_valid: false` and a note. The cleanup field comes from the
process layer for the provider and from the probes.

If a provider writes its final result and then does not exit, the run ends
`timed_out` (or `interrupted`), keeps the answer in `text`, and says so in a note.

## Structured answers

`answer_schema` is not passed to the provider as an option. The prompt is
followed by a block asking for one JSON value that satisfies the schema, with no
code fence. The final text is parsed (one surrounding code fence is tolerated)
and validated. The value goes in `structured`, the verdict in `structured_valid`.
An answer of JSON `null` is left in `text` only, with a note.

## Instructions and standard input

Every worker gets a fixed preamble (complete only the assignment, follow
repository instructions, the parent conversation is unavailable, report blockers,
do not delegate), then the request's `instructions`. They go to Claude Code in
`--append-system-prompt` and to Codex in `developer_instructions`. If that
argument would exceed 120 KiB, the instructions are written to standard input
before the assignment instead, and a note says so. The prompt is always on
standard input, never in arguments.

## Claude Code

Command shape, in order:

```text
claude -p --output-format stream-json --verbose
  --permission-mode dontAsk --permission-prompts none
  --strict-mcp-config --mcp-config '{"mcpServers":{}}'
  --setting-sources "" --settings <json>
  --add-dir <cwd> [--add-dir <directory> ...]
  --disable-slash-commands --no-chrome
  --tools <names> --allowedTools <rules>
  [--resume <id>] [--no-session-persistence]
  [--model <m>] [--effort <low|medium|high>]
  --append-system-prompt <text>
  [native_args]
```

Environment set by the adapter: `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1` and,
when the command guard is installed, `DELEGA_EXACT_BASH_GRANTS` (a JSON array of
the allowed commands). `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD` is not
set: a directory listed in `permissions.directories` is for reading and does not
contribute instruction files.

`--permission-mode dontAsk` with `--permission-prompts none` means anything not
granted is denied at once. Denials come back in the result event's
`permission_denials` and become `permissions.denied` entries such as
`Bash: make install`.

### Permission mapping

`CWD` is the resolved working directory. A rule path is written `//abs/path/**`.

| Mode | `--tools` | `--allowedTools` | Settings |
| --- | --- | --- | --- |
| `read` | `Read,Glob,Grep` | `Read(//CWD/**)`, one `Read(//DIR/**)` per directory | hooks off |
| `write` | `Read,Glob,Grep,Edit,Write` | the `read` rules plus `Edit(//CWD/**)`, `Write(//CWD/**)` | hooks off |
| `full` | all nine built-in tools: `Read,Glob,Grep,Bash,Edit,Write,NotebookEdit,WebFetch,WebSearch` | the same nine, unscoped | hooks off |

`Glob` and `Grep` are in `--tools` and are not allow rules. Listed bare they
let `Grep` read outside the working directory; unlisted, Claude Code applies the
scoped `Read` rules to them, so they search only the working directory and the
listed directories.

### Commands and directories

- `permissions.commands` (`read` and `write`): `Bash` is added to `--tools`, one
  `Bash(<command>)` rule per command is added to `--allowedTools`, and a blocking
  `PreToolUse` hook is installed in `--settings`. The hook allows a `Bash` call
  only when its command equals a granted command byte for byte, and denies a call
  that asks to run in the background or to disable the provider's sandbox. In
  `full`, commands are not listed: every command is allowed.
- The hook is a program the caller supplies in `ProviderOptions::hook`
  (`HookCommand { program, args }`). `provider::hook::run` is the entry point it
  calls; the `delega` binary exposes it as the hidden subcommand
  `command-guard`, and `delega run` passes its own executable path and that
  subcommand as the hook. Without a hook, `permissions.commands` is not granted
  at all and a note says so. Claude Code must be 2.1.325 or later for the hook
  interface.
- `permissions.directories`: each gets `--add-dir` and a `Read(//DIR/**)` rule.
  It is readable, not editable: no `Edit` or `Write` rule names it.

### Text refused because it could change a rule

Rules are written into one comma-separated `--allowedTools` value, so text that
could end or split a rule, or be read as a pattern, is refused before start. The
same text is accepted for Codex, which has no rule text.

- **Commands** (kind `unsupported_command`): any command containing `(`, `)`,
  `,`, `*`, `\`, a control character (line breaks, tabs and NUL included), U+2028
  or U+2029, or any fullwidth or halfwidth form (U+FF00 to U+FFEF, which includes
  the fullwidth comma, asterisk and parentheses); a command with leading or
  trailing whitespace; and a command with a tool-name word (below). Everything
  else is allowed.
- **The working directory and `permissions.directories`** (kinds `cwd_unusable`
  and `directory_unusable`): the resolved path may contain only ASCII letters and
  digits, the characters ` / . _ - + @ = : % ~` (the space is included), and
  letters of other scripts (accented Latin, Cyrillic, CJK and so on). Non-ASCII
  digits are refused, and so are letters in the blocks that fold to punctuation
  or digits under compatibility normalisation: super and subscripts, letterlike
  symbols and number forms (Roman numerals), enclosed alphanumerics and their
  supplement, enclosed and compatibility CJK, small forms, and fullwidth and
  halfwidth forms. No path is escaped. In `full` no path rules are written, so
  these paths are not checked.
- **Tool-name words**, for both: the Claude Code help says `--allowedTools` is
  "comma or space-separated", so a reader may split on whitespace without
  tracking parentheses. Text whose words, after the first, include (ignoring
  case, with every Unicode whitespace character as a delimiter) a built-in tool
  name (`Read`, `Glob`, `Grep`, `Bash`, `Edit`, `Write`, `NotebookEdit`,
  `WebFetch`, `WebSearch`, `Task`, `Agent`, `TodoWrite`, `MultiEdit`,
  `NotebookRead`, `LS`, `BashOutput`, `KillShell`, `SlashCommand`,
  `ExitPlanMode`), a word starting with a tool name and `(`, or a word starting
  with `mcp__`, is refused. The first word is attached to the rule's own
  `Tool(` and cannot stand alone, so `ls` and `make read-docs` are fine and
  `make read` and `x Edit y` are not. A command or directory name with a space
  that is accepted is read as split fragments by such a reader, which fails
  closed (the rule matches less) and never adds a tool.

### What cannot be enforced

- `--setting-sources ""` means Claude Code loads no user, project or local
  settings file. Not applied to the worker: the user's and project's
  `permissions.deny` rules, hooks, `env`, `apiKeyHelper`, plugins, skills, MCP
  servers and settings-defined instructions. Every Claude run carries a note
  saying the user's and project's settings, including deny rules, were not
  applied. The earlier project merged those deny rules into the launch; this
  layer does not (see [Not done](#not-done)).
- Which instruction files Claude Code still reads is not controlled by delega.
  Reference directories contribute none (the variable that would load them is
  not set). Whether the working directory's own `CLAUDE.md` is read with empty
  settings sources is the provider's behaviour and is listed as unverified.
- A working directory of `/` or the home directory makes the working-directory
  rules cover everything below it. The run is not refused; a note warns.
- `full` is the nine built-in tools. Sub-agents, MCP tools and background tasks
  stay off, and a note says so.
- `NotebookEdit` is not granted in `write`.
- The provider's own managed policy is not inspected.

### Resume, ephemeral, effort

`session.resume` is `--resume <id>`. `session.ephemeral` is
`--no-session-persistence`, and the result then has no `session_id`. `effort`
is `--effort`. `model` is `--model`. The request has no turn or budget fields, so
`--max-turns` and `--max-budget-usd` are not set; the time limit is the bound,
and both can be passed as `native_args`.

### Stream

Events are one JSON object per line. `session_id` is taken from any event. The
`result` event gives the answer (`result`), success (`subtype == "success"` and
`is_error` not true), errors, `usage` (`input_tokens` plus
`cache_creation_input_tokens`, reported as `input_tokens`; `output_tokens`;
`cache_read_input_tokens` as `cached_tokens`), `total_cost_usd` and
`permission_denials`. Messages with `parent_tool_use_id` are sub-agent messages
and are not used as the worker's text. When the result carries no text, the last
assistant message is used and a note says so.

### `native_args` allowlist

| Option | Value |
| --- | --- |
| `--max-turns` | a whole number of at least 1 |
| `--max-budget-usd` | a decimal number, at most 10 characters |
| `--fallback-model` | a model-like name: letters, digits, `. _ : / -`, starting with a letter or digit |

Both `--flag value` and `--flag=value` are accepted.

## Codex CLI

Command shape, in order:

```text
codex --disable plugins --disable apps --disable hooks --disable multi_agent
  -c approval_policy="never"
  -c sandbox_mode="<read-only|workspace-write|danger-full-access>"
  -c allow_login_shell=false
  [-c sandbox_workspace_write.network_access=false
   -c sandbox_workspace_write.exclude_slash_tmp=true
   -c sandbox_workspace_write.exclude_tmpdir_env_var=true]   (write only)
  [-c model_reasoning_effort="<low|medium|high>"]
  -c developer_instructions="<text>"
  -c mcp_servers={...every listed server disabled...}
  [native_args]
  exec [resume <id>] --ignore-user-config --json --skip-git-repo-check
  [--ephemeral] [--model <m>] -
```

The adapter sets no environment variables.

### Login shell

Every Codex launch, in all modes, passes `-c allow_login_shell=false`. Without
it, the shell commands Codex runs for the worker start as login shells and load
the user's shell profile, so a worker's `env` listed names that delega did not
pass, including secret-looking ones. With it, commands see the environment
delega passed plus what Codex adds. Verified live on codex-cli 0.170.0 on
2026-11-18: with the setting, `env` inside a worker listed 28 names, none
secret-looking and none from a shell profile; without it, 63 names, 10
secret-looking, although delega passed 9.

`shell_environment_policy.inherit="core"` is deliberately not used. It would
also drop variables the caller named with `--env`, which the caller asked the
worker to have.

### Permission mapping

| Mode | Sandbox | Approval | Notes |
| --- | --- | --- | --- |
| `read` | `read-only` | `never` | Writes are blocked. |
| `write` | `workspace-write`, network off, `/tmp` and `$TMPDIR` excluded | `never` | Writes limited to the working directory. |
| `full` | `danger-full-access` | `never` | No sandbox. |

`approval_policy="never"` means nothing is ever asked: an action the sandbox
does not allow fails.

### Wider than requested

In `read` and `write`, Codex does more than the request asks. The same note,
starting `wider than requested`, is on every Codex `read` and `write` result and
in `dry_run`, whether or not the request set `commands` or `directories`:

- Codex can read any file the user can read, including outside the working
  directory and `permissions.directories`. The request says "read the working
  directory" and the listed directories.
- Codex can run any command that works inside the sandbox: in `read`, any command
  that needs no write access and no network access; in `write`, also commands
  that write inside the working directory. The request says "no commands except
  those in `permissions.commands`".
- `permissions.commands` is not turned into any setting, and
  `permissions.directories` is not added as a readable (or writable) directory;
  `--add-dir` is not used.
- Writes are stricter than `write` strictly needs: temporary directories and the
  network are excluded.
- `full` has no sandbox at all.

The mapping is not stricter than the request here because Codex has no setting
that would make it so. Use Claude Code when `commands` and `directories` must be
enforced.

### What cannot be enforced

- Reads are not limited to the working directory or the listed directories.
- Commands are not limited to the list.
- Denials are reported only for commands the stream marks `declined`. Other
  blocked actions appear as failed commands and are not in `permissions.denied`.
  A note says so on every run.
- `--ignore-user-config` ignores the user's Codex configuration, but Codex may
  still load its own `AGENTS.md` files.
- The provider's own managed policy is not inspected.

### Stream

`thread.started.thread_id` is the session id. A message item that is not marked
`commentary` is the answer. `turn.completed` is success and carries `usage`
(`input_tokens`, `output_tokens`, `cached_input_tokens`,
`reasoning_output_tokens`); `turn.failed` is failure with its message. `error`
events are kept: they fail the run only if no terminal event follows.

### `native_args` allowlist

| Option | Value |
| --- | --- |
| `-c` or `--config` | `model_verbosity=low\|medium\|high` or `model_reasoning_summary=auto\|concise\|detailed\|none` |

Any other `-c` key or value is refused, because configuration keys can change
the sandbox, approval policy or working directory.

## Rejected `native_args`

Anything not on the allowlist is `refused` before start with kind
`native_arg_rejected`. The message names the option and the position, never the
value. Options known to change permissions, sandboxing, the working directory,
the configuration location, the session or the output format get a message saying
so (for example `--permission-mode`, `--add-dir`, `--settings`, `--output-format`,
`--sandbox`, `--full-auto`, `--cd`, `--json`, `--profile`). A value without an
option before it is refused.

## Environment policy

The policy is an allowlist. The provider receives only the variables below,
taken from the parent environment when it has them, then `env_set`, then the
adapter's own variables. Everything else is withheld, whatever its name.

1. **Base.** `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LANGUAGE`,
   `TERM`, `TMPDIR`, `TZ`, the locale categories `LC_ALL`, `LC_COLLATE`,
   `LC_CTYPE`, `LC_MESSAGES`, `LC_MONETARY`, `LC_NUMERIC`, `LC_TIME`,
   `LC_ADDRESS`, `LC_IDENTIFICATION`, `LC_MEASUREMENT`, `LC_NAME`, `LC_PAPER` and
   `LC_TELEPHONE` (listed by name, so another name that starts with `LC_` is
   withheld), the base directory variables
   `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME` and `XDG_STATE_HOME`,
   and the switches `NO_COLOR` and `CI`. The `XDG_*` variables are locations,
   so a worker finds the same configuration and cache as the caller's shell
   would. `NO_COLOR` and `CI` are switches tools read to decide how to print.
   None of these carries a secret.
2. **Network.** `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY` in upper
   and lower case, `SSL_CERT_FILE`, `SSL_CERT_DIR` and `NODE_EXTRA_CA_CERTS`.
3. **The provider's own.** For Claude Code, `CLAUDE_*` and
   `ANTHROPIC_*`; for Codex, `CODEX_*` and `OPENAI_*`; in each case minus the
   variables that describe the calling session (group below). The other
   provider's variables are not passed, so a Claude Code worker does not see
   Codex credentials and the reverse.
4. **Named by the caller.** `ProviderOptions::env_pass`, which the CLI fills
   from `--env NAME` (repeatable, short `-e`). A name that is not set in the parent
   environment is ignored in a run and listed as "named but not set" by a dry
   run. A name that is not a valid variable name (letters, digits and
   underscore, not starting with a digit), a calling-session variable or a
   `DELEGA_*` variable is `refused` before start, with kind `invalid_env_name`
   or `forbidden_env_name`; the message never repeats an invalid name, because
   a mistaken `NAME=value` would put a value in it. A named variable may belong
   to the other provider. Naming a variable whose name looks like a secret (see
   below) is allowed, because the caller asked, and the result carries a
   `provider_notes` entry naming each such variable. A name that group 1, 2
   or 3 already passes is described under that group, not here, and gets no
   note. A name given in both forms is set (group 5) and listed and noted only
   as set. A name delega itself sets for the provider (group 5) is `refused`
   with kind `forbidden_env_name`, in either form, instead of being dropped.
5. **Set by the caller or the adapter.** `env_set`, which the CLI fills from
   `--env NAME=VALUE`, sets variables after the rules above, whatever the parent
   environment holds. The names are checked as in group 4: an invalid,
   calling-session or `DELEGA_*` name is `refused`, and a secret-looking name
   gets a note. Prefer `NAME` (group 4) for secrets on the command line, since a
   value there is visible in the process table and shell history. The adapter's variables (`CLAUDE_CODE_DISABLE_BACKGROUND_TASKS`,
   `DELEGA_EXACT_BASH_GRANTS`) cannot be overridden.

Never passed, even when named: `DELEGA_*`, and the variables that describe the
calling session, so a worker started from inside a Claude Code session does not
take that session's identity. A name is a calling-session variable when it is
one of `AI_AGENT`, `CLAUDECODE`, `CLAUDE_CODE_CHILD_SESSION`,
`CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_EXECPATH`,
`CLAUDE_CODE_SESSION_ATTENDED`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_EFFORT`,
`CLAUDE_PID`, `CLAUDE_CODE_MESSAGING_SOCKET`; or starts with `CLAUDE_CODE_` and
contains `SESSION`, `MESSAGING` or `BRIDGE`, has `SSE` as a whole word between
underscores (`CLAUDE_CODE_SSE_PORT`), or ends in `_PORT`, `_SOCKET` or `_PID`;
or is one of the variables Codex sets for the commands it runs, which describe
a calling Codex session or tool call: `CODEX_SANDBOX`,
`CODEX_SANDBOX_NETWORK_DISABLED`, `CODEX_SESSION_ID`, `CODEX_THREAD_ID`,
`CODEX_TOOL_CALL_ID`, `CODEX_CI`, `CODEX_VERSION` and
`CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS` (seen inside a worker's command
environment on codex-cli 0.170.0). These are withheld from workers of both
providers. Other `CLAUDE_*` and `ANTHROPIC_*` names are configuration and pass
to a Claude Code run: an API key or base URL, `CLAUDE_CONFIG_DIR`,
`CLAUDE_CODE_OAUTH_TOKEN`. `CODEX_HOME`, `OPENAI_API_KEY`, `OPENAI_BASE_URL`
and other account and configuration names pass to a Codex run.

Consequences for a caller:

- Cloud credentials for Claude Code on Bedrock or Vertex (`AWS_*`, `GOOGLE_*`
  and the like) are not passed by default. Name each one with `--env NAME`.
- Variables that a command the worker may run needs are not passed either:
  a toolchain home such as `JAVA_HOME`, `CARGO_HOME` or `GOPATH`, `GIT_*`,
  `GH_TOKEN`, `SSH_AUTH_SOCK`, a package registry token, `DATABASE_URL`. Name
  what the task needs. `COLORTERM`, `FORCE_COLOR` and `XDG_RUNTIME_DIR` are not
  on the base list either.
- A variable whose name is not valid UTF-8 is never passed.

A name **looks like a secret** when, in upper case, it contains `SECRET`,
`TOKEN`, `PASSWORD`, `PASSWD`, `CREDENTIAL`, `PRIVATE_KEY`, `ACCESS_KEY` or
`CONNECTION_STRING`, ends in `_KEY`, `_PAT`, `APIKEY`, `_URL`, `_URI` or `_DSN`,
or is `SSH_AUTH_SOCK`. This heuristic is not part of what is passed. It decides
two things: which passed values are removed from text the provider writes (see
[Text from the provider](#text-from-the-provider)), together with the values of
the provider's own variables, and which explicitly named variables get a note.

What is visible, by names and counts only:

- `dry_run().env_names` lists every name that would be passed, and
  `dry_run().env` (`EnvReport`) groups them as `base`, `network`, `provider`,
  `named`, `set_by_caller` and `set_by_delega`, lists `named_not_set`, and gives `withheld_count`, the
  number of the caller's variables that are not passed. The names of withheld
  variables are not reported anywhere.
- Every result carries one `provider_notes` entry of the form `N environment
  variables of the caller were withheld; M were passed`. `M` counts every
  variable the provider receives, including those set by the caller and the
  adapter.

`ProviderOptions`'s `Debug` and the `ResolvedEnv` type show names only. The
prompt, the instructions and variable values are not in a dry run. In a result,
the only text that can carry a value is text the provider wrote (an error
message or the standard error excerpt), which is redacted as described under
[Text from the provider](#text-from-the-provider), and the answer, which is the
provider's own output.

## Verified against the real CLI

Verified on 2026-11-18 with Claude Code 2.1.328 and codex-cli 0.170.0:

- A scoped `Read(//dir/**)` allow rule scopes reads under `dontAsk`.
- `Grep` and `Glob` follow the scoped `Read` rules when they are not listed as
  allow rules. Listed bare, `Grep` read a file outside the working directory.
- `--add-dir` makes a directory readable and not writable.
- The hook settings shape works: the guard is called and its denial is honoured.
- The `--version` output of both CLIs is parsed.
- `--effort low` and the Codex effort key are accepted.
- `--no-session-persistence` and Codex `--ephemeral` are accepted.
- Both providers exit 0 after a successful result.
- The Codex `workspace-write` sandbox blocks writes outside the working
  directory.
- Session resume works on both.

## Unverified against the real CLI

The fake providers in the tests copy the stream formats of the earlier
project's fakes. The fake Claude Code also honours its tool, rule and hook
arguments, and the fake Codex its sandbox flag, so that the tests show
enforcement through `run`; that proves the plumbing against the behaviour
assumed here, not the real CLIs. Nothing in this table was run against the real `claude` or `codex`. The choices below are not
confirmed and were made conservatively. A live run per provider should settle
each.

| Assumption | Conservative choice |
| --- | --- |
| `full` for Claude Code needs no `--permission-mode bypassPermissions` | The nine built-in tools are allowed under `dontAsk`, the mechanism the earlier project already used. |
| Codex's own default exclusion of secret-looking variable names, which applies to the environment of the commands it runs, does not drop a variable the caller passed with `--env` | Not tested. If it does, a named secret-looking variable reaches Codex but not its commands. |
| Codex honours `sandbox_workspace_write.network_access`, `exclude_slash_tmp` and `exclude_tmpdir_env_var` given with `-c` | They only narrow the sandbox. An unknown key would fail the run. |
| A declined command is `item.completed` with `status: "declined"` | Only those are listed in `permissions.denied`, and a note says other blocks are not. |
| `model_verbosity` and `model_reasoning_summary` are accepted config keys with the listed values | They are the only `-c` keys allowed. |
| Neither provider's schema option (`--json-schema`, `--output-schema`) is needed | Not used. The schema is in the prompt and the answer is validated here. |
| `--effort low\|medium\|high` and `model_reasoning_effort` accept the same words | The earlier project passed them. Its extra levels `xhigh` and `max` are not in the request. |
| With empty settings sources Claude Code still reads the working directory's own `CLAUDE.md` | Not controlled. A note on every run says what delega does and does not load. |
| A Claude failure with `permission_denials` and no `error` or `api_retry` event is a stop caused by the denial | Anything else with a denial is `failed`. |
| Codex words a usage or rate limit failure with "usage limit", "rate limit", "quota" or "context window", and Claude Code's failed `errors` name the denied tool or speak of permission when a denial stopped it | Matching is by those words. Anything else with a denial is `failed`, never `refused`. |
| Claude Code names `error_max_turns` and `error_max_budget_usd` as result subtypes | They become the `provider_turn_limit` and `provider_budget_limit` error kinds. |

## Not done

- **User deny rules and instructions for Claude Code.** The earlier project
  read the user's and project's `permissions.deny` arrays and the user's
  `CLAUDE.md` and copied them into the launch, because `--setting-sources ""`
  otherwise drops them. That reads the user's own files and is a separate
  decision about what may be read. This layer does not read them; every Claude
  result says the settings were not applied.
- **Command shape in the result.** The contract's `provenance` has no field for
  it. `dry_run` reports the shape; a run records the executable and version.
- **Live asking** (`on_request: ask`), as in [api.md](api.md).
- **Stopping at the final result.** A provider that writes its result and then
  does not exit holds the run until the time limit.
- **Usage during a run.** Usage is taken from the terminal event only.

# Providers

How delega starts Claude Code and Codex CLI and what each request field turns
into. The code is `crates/delega-core/src/provider/`. The request and result
fields are defined in [api.md](api.md).

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

Numeric limits are in `crates/delega-core/src/provider/limits.rs`.

## Finding the executable

In order:

1. `ProviderOptions::executable`.
2. `DELEGA_CLAUDE_BIN` or `DELEGA_CODEX_BIN` in the parent environment (empty
   counts as unset).
3. `ProviderOptions::claude_executable` or `codex_executable`, which `delega`
   fills from `[providers.claude] executable` or `[providers.codex] executable`
   in the user-level [configuration file](api.md#configuration-file). Doctor
   reports it as `config`.
4. `claude` or `codex` on the absolute entries of `PATH`. Relative and empty
   entries are skipped.

A path from 1, 2 or 3 must be absolute and an executable file. Otherwise the run is
`refused` with kind `provider_not_found`, and the message names the variable to
set. The path used is `provenance.provider_executable`.

## Diagnostics

See [Doctor](api.md#doctor) for report fields, problem kinds and exit status.
Doctor checks both providers without starting workers. Claude version probe
errors appear as `version_probe_failed` or `version_probe_timed_out`; a run
requiring the command guard can report `unsupported_provider_version`.

## Run sequence

1. Validate the request (`Request::validate`), the `native_args`, and the
   `answer_schema` (it must compile). Resolve `cwd` and `permissions.directories`
   (each must exist). Look up the executable. For Claude Code, read the user's
   deny rules and check for exclusive managed MCP. Any refusal at this stage
   leaves the provider unstarted.
2. Run `<executable> --version` (5 s). The first line, up to 100 characters, is
   `provenance.provider_version`. For Claude Code with `permissions.commands`
   granted, a version below 2.1.325 or an unreadable one is `refused` with kind
   `unsupported_provider_version`, or `failed` with that kind when the probe's
   processes could not be confirmed stopped.
3. Codex only: run `<executable> --disable ... mcp list --json` (10 s) and build
   an override that disables every server it lists. A failure, invalid JSON or a
   transport that cannot be disabled is `failed` with kind
   `provider_preparation_failed`, and the run does not start.
4. Start the provider with the prompt on standard input. The time limit is
   `timeout_secs` minus the time steps 2 and 3 took. If it is used up, the run is
   `timed_out` without starting.

Probes use the worker environment and working directory. Each captures up to
2,000,000 bytes of stdout and 16 KiB of stderr. Probe cleanup is included in
`cleanup`. See `crates/delega-core/src/provider/run.rs`.

## Progress events

Set `ProviderOptions::progress` to a `ProgressSink` that returns promptly.
Probes emit no progress. See [Reading](supervisor.md#reading) for supervisor
activity and partial answers.

| Event | Contents |
| --- | --- |
| `Started` | The provider is about to start. |
| `Provider { kind, summary }` | Provider event type and a short summary, such as message length, tool name, item type and status, or up to 80 characters of redacted error text. |
| `Message { text }` | With `progress_text` enabled, redacted agent text up to 4,000 characters after its `Provider` event. Off by default and in `delega run`; enabled by the supervisor. |
| `Denied { request }` | An entry from `permissions.denied`, including command or path, reported after provider exit. |
| `Dropped { chunks }` | Number of output chunks missed by the tap, also in `provider_notes`. |
| `Finished { outcome }` | Last event, including on refusal before start. |

A slow `ProcessSpec::stdout_tap` can lose chunks. A panicking tap stops receiving
them. The owner waits up to 250 ms for the tap at run end.
See `crates/delega-core/src/provider/progress.rs`.

## Outcomes

See [Outcomes](api.md#outcomes) for the shared contract and
[Command line](api.md#command-line) for exit status.

| Outcome | When |
| --- | --- |
| `completed` | The provider reported success, exited with code 0, and gave answer text. With `answer_schema`, the text parsed as JSON. A denial that did not stop the run is listed in `permissions.denied`. |
| `failed` | Provider error event or result (`provider_error`, or `provider_turn_limit` or `provider_budget_limit` when Claude Code names a limit, or, when the failure says so, `provider_usage_limit`, `provider_auth`, `provider_billing`, `provider_capacity`, `provider_model_unavailable` or `provider_context_exhausted`, see [Provider failure kinds](#provider-failure-kinds)), non-zero exit (`provider_exit`, with a bounded, redacted excerpt of standard error, see [Text from the provider](#text-from-the-provider)), success followed by a non-zero exit, no terminal event and a clean exit (`no_final_message`), output that was not events (`malformed_output`), success with no answer text, `answer_not_json`, a resumed run that would otherwise have completed but whose stream reported another session than `session.resume` (`session_mismatch`; any other outcome stands with a note), a resumed run whose session the provider could not find (`session_not_found`), a failed preparation step, a failure to start. |
| `timed_out` | The time limit passed. `text` holds any partial answer. |
| `interrupted` | `interrupt()` was called, or the handle or future was dropped. |
| `refused` before start | See steps 1 and 2 above. `timing.started_at` is absent. |
| `refused` after start | The run stopped because of a denial, by the rule below. Kind `permission_denied`. |

After start, `permission_denied` requires a terminal failure attributable
to a denial. For Claude Code, the failure must name the denied tool or indicate
permission trouble if it includes error text. A separate error, retry, or
activity after the denied tool result makes it `failed`. For Codex, the last
item must be a declined command, with no separate error. Named provider
failures and limits stay `failed`. The denials remain in `permissions.denied`
in either case. A failed or refused-after-start run whose process exited
includes `provider_exit_code`.

The first terminal event decides the answer. Additional terminal events and
invalid UTF-8 (decoded with replacement characters) add notes. A provider
that sends a result but stays running can still end `timed_out` or
`interrupted`, with the answer in `text` and a note.

On resume, a different session ID anywhere in captured root events changes
`completed` to `failed` with `session_mismatch` and keeps the partial answer
in `text`. Other outcomes keep their kind and get a note naming both IDs.
A missing ID is allowed. Ephemeral runs skip session checks. Fresh runs
report up to 8 distinct IDs in a multiple-session note and count additional
IDs. The result's `session_id` comes from events up to the first terminal
event. A provider that reports the requested session missing can yield
`session_not_found` ([verified messages](#verified-against-the-real-cli)).

| Capture limit | Effect |
| --- | --- |
| `max_output_bytes`, default 16 MiB | Standard output kept from the start. Truncation adds a note and limits session checks. |
| 8 MiB per output line | Longer lines are skipped and counted. |
| 64 denials | Additional denials are counted in a note. |
| 500 characters per denial target, 300 per reason | Longer text is clipped. |

See `crates/delega-core/src/provider/settle.rs`.

### Error kinds

`invalid_request`, `invalid_answer_schema`, `native_arg_rejected`,
`cwd_unusable`, `directory_unusable`, `unsupported_command`,
`permissions_not_enforceable`, `permissions_not_supported`, `provider_not_found`,
`unsupported_provider_version`, `invalid_env_name`,
`forbidden_env_name`, `settings_unreadable`, `deny_rules_over_limit`,
`managed_mcp_exclusive` (all refused before start);
`provider_error`, `provider_turn_limit`, `provider_budget_limit`, `provider_usage_limit`,
`provider_auth`, `provider_billing`, `provider_capacity`,
`provider_model_unavailable`, `provider_context_exhausted`, `provider_exit`, `no_final_message`, `malformed_output`, `answer_not_json`,
`session_mismatch`, `session_not_found`, `provider_preparation_failed`, `spawn_failed`, `process_signaled`,
`wait_failed`, `internal_error` (failed); `permission_denied` (refused after
start); `timed_out`; `interrupted`.

### Provider failure kinds

A failed run whose provider failure says why gets a kind for the reason instead
of `provider_error`:

| Kind | Meaning |
| --- | --- |
| `provider_usage_limit` | The account's usage limit or quota is used up. Retrying before it resets fails again. |
| `provider_auth` | The credentials or the account cannot be used: an invalid key, an organisation not allowed, an account on hold, a verification needed, a cloud credential error, or HTTP 401 or 403. |
| `provider_billing` | A billing problem, such as no credit left. |
| `provider_capacity` | The provider is rate limiting or overloaded (HTTP 429, 503, 529). Retrying later can work. |
| `provider_model_unavailable` | The requested model does not exist or this account cannot use it. |
| `provider_context_exhausted` | The conversation no longer fits the model's context window. |

Classification applies to failures. `session_not_found` and Claude's
turn and budget limits take precedence. For overlapping matches at the same
classification stage, the order is usage, billing, auth, model, context and
capacity. An unmatched failure keeps `provider_error`.
See [Claude Code stream](#stream) and [Codex stream](#stream-1).

### Text from the provider

Provider errors and progress text replace common credential shapes and passed
values of provider or [secret-looking variables](#environment-policy) with
`[redacted]`. Values shorter than 6 characters are excluded from value
matching. Recognised shapes include API keys, forge and Slack tokens, AWS
access key IDs, JWTs, Bearer tokens and private key blocks. Answer text and
denied commands or paths are shown as provided, subject to capture limits.

| Limit | What you see |
| --- | --- |
| 64 KiB | Standard error captured from the end. |
| 8 KiB, then 300 characters | Redacted standard error excerpt from the end, joined to one line. A character cut adds `... `. If capture was cut and no excerpt remains, the error says so. |
| 500 characters | Redacted provider error message from the start. |

Credential matching is heuristic. A credential cut by the capture limit with
more than 8 KiB remaining in capture, or a private key with overlapping
markers, may still expose text. Configured
secret values overlapping `...` or `[redacted]` are unsupported.
See `crates/delega-core/src/provider/redact.rs` and
`crates/delega-core/src/provider/settle.rs`.

## Structured answers

With `answer_schema`, delega asks for a JSON value in the prompt and accepts
one surrounding code fence. Invalid JSON fails with `answer_not_json`.
The parsed value goes in `structured` and the verdict in `structured_valid`.
A schema mismatch is `completed` with `structured_valid: false` and a note
(validation message up to 300 characters). JSON `null` stays in `text` with
a note. See [The result](api.md#the-result).

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
  --add-dir <cwd> [--add-dir <directory> ...]   (directories: not in build)
  --disable-slash-commands --no-chrome
  --tools <names> --allowedTools <rules>
  [--resume <id>] [--no-session-persistence]
  [--model <m>] [--effort <low|medium|high>]
  --append-system-prompt <text>
  [native_args]
```

The adapter sets `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1`,
`CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` and `CLAUDE_CODE_DISABLE_CLAUDE_MDS=1`.
With the command guard, it sets `DELEGA_EXACT_BASH_GRANTS` to a JSON array of
exact commands. It leaves `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD` unset.

`--permission-mode dontAsk` with `--permission-prompts none` means anything not
granted is denied at once. Denials come back in the result event's
`permission_denials` and become `permissions.denied` entries such as
`Bash: make install`.

### Permission mapping

`CWD` is the resolved working directory. A rule path is written `//abs/path/**`.

| Mode | `--tools` | `--allowedTools` | Settings |
| --- | --- | --- | --- |
| `read` | `Read,Glob,Grep` | `Read(//CWD/**)`, one `Read(//DIR/**)` per directory | hooks off |
| `write` | `Read,Glob,Grep,Edit,Write` | the `read` rules plus `Edit(//CWD/**)` | hooks off |
| `build` | `Read,Glob,Grep,Edit,Write,Bash` | the `write` rules; no `Bash` rule | hooks off, `sandbox`; directories without `--add-dir` (see [Build](#build)) |
| `full` | all nine built-in tools: `Read,Glob,Grep,Bash,Edit,Write,NotebookEdit,WebFetch,WebSearch` | the same nine, unscoped | hooks off |

`Glob` and `Grep` are in `--tools` and are not allow rules. Listed bare they
let `Grep` read outside the working directory; unlisted, Claude Code applies the
scoped `Read` rules to them, so they search only the working directory and the
listed directories.

In `write` and `build`, the scoped `Edit` rule covers both the `Edit` and
`Write` tools.

### Build

| `sandbox` setting | Effect |
| --- | --- |
| `enabled: true` | Runs Bash in Claude Code's OS sandbox. Network is denied under `dontAsk`. |
| `failIfUnavailable: true` | Startup fails if the sandbox is unavailable. macOS has it built in; Linux requires `bubblewrap` (`bwrap`) and `socat`. `delega doctor` reports `sandbox_unavailable` when they are not on the worker's `PATH` ([Doctor](api.md#doctor)). |
| `allowUnsandboxedCommands: false` | Failed commands stay inside the sandbox. |
| `autoAllowBashIfSandboxed: true` | Claude Code approves sandboxed commands without a Bash allow rule. Some commands, including `eval` and the probed `git -c … commit`, can still be denied. |

Commands can read outside the working directory and listed directories, so
the run carries a `wider than requested` note. Commands can write in the
working directory and Claude Code's per-user `TMPDIR` (`/tmp/claude-<uid>`,
shared with that user's Claude Code sessions). `git add` and `git commit` can
write to the main checkout's Git metadata from a worktree.

Listed directories get a scoped Read rule without `--add-dir`. A directory
inside a writable location stays writable by commands.
`permissions.commands` must be empty. Managed sandbox settings can override
these grants and restrictions ([Managed settings](#managed-settings)).

### Commands and directories

- `permissions.commands` (`read` and `write`): `Bash` is added to `--tools`, one
  `Bash(<command>)` rule per command is added to `--allowedTools`, and a blocking
  `PreToolUse` hook is installed in `--settings`. The hook allows a `Bash` call
  only when its command equals a granted command byte for byte, and denies a call
  that asks to run in the background or to disable the provider's sandbox. In
  `full`, the exact-command list and delega's command guard are left out; the
  user's deny rules and any provider-managed policy can still apply.
- The hook is a program the caller supplies in `ProviderOptions::hook`
  (`HookCommand { program, args }`). `provider::hook::run` is the entry point it
  calls; the `delega` binary exposes it as the hidden subcommand
  `command-guard`, and `delega run` passes its own executable path and that
  subcommand as the hook. Without a hook, `permissions.commands` is not granted
  at all and a note says so; with `permissions.strict` the run is refused before
  start with `permissions_not_enforceable` instead. Claude Code 2.1.325 or later supports the hook interface. The guard has
  a 5 s timeout and a 2,000,000-byte input limit.
- `permissions.directories`: each gets `--add-dir` and a `Read(//DIR/**)` rule.
  The `Edit` rule covers a listed directory only if it lies inside the working
  directory. In `build`, listed directories get their `Read` rule without
  `--add-dir` and stay writable by commands when they lie inside a writable
  location ([Build](#build)).

### Text refused because it could change a rule

Claude Code refuses command or resolved path text that can change the meaning
of an inline permission rule. Codex has no corresponding rule text.

| Input | Refusal |
| --- | --- |
| Exact command | `unsupported_command` for `(`, `)`, comma, `*`, backslash, control characters, U+2028, U+2029, fullwidth or halfwidth forms (U+FF00–U+FFEF), leading or trailing whitespace, or a tool-name word after the first word. |
| Working directory or listed directory | `cwd_unusable` or `directory_unusable`. Scoped paths allow ASCII letters and digits, space, `/ . _ - + @ = : % ~` and letters from other scripts except compatibility forms that can fold to punctuation or digits. Tool-name words after the first word are refused. `full` skips these rule-text checks. |

Tool-name matching ignores case and splits on Unicode whitespace. It covers
names in Claude's rule validator, including `Read`, `Task` and `Agent`,
those names followed by `(`, and words beginning with `mcp__`.
For example, `make read-docs` passes and `make read` is refused.
See `crates/delega-core/src/provider/claude.rs`.

### Deny rules

Claude workers receive the user's `permissions.deny` in the inline
`--settings` JSON, including in `full`. Hooks, `env`, `apiKeyHelper`,
plugins, skills, MCP servers, instructions, `allow` and `ask` from those
files are excluded. Codex reads no Claude settings and its dry run has an
empty `deny_rules`.

| Order | File |
| --- | --- |
| 1 | `$CLAUDE_CONFIG_DIR/settings.json` or `$HOME/.claude/settings.json` from the worker environment. A note appears if neither directory is set. |
| 2 | `.claude/settings.json` under the working directory. |
| 3 | `.claude/settings.local.json` under the working directory, then the nearest Git root, then the main checkout for a linked worktree. |

Missing files contribute nothing. Empty or whitespace-only files have no
rules. A UTF-8 BOM is accepted. User and working-directory files may link to
regular files. Additional Git roots at the user's home, with a symlinked
`.git`, or with foreign-owned entries are skipped. Links to `.claude` or
its settings file at those roots require user ownership of the link, its
containing directory and its target, plus no group or other write access to
the containing directory. A skipped source or an unresolved worktree checkout
gets a note naming its path and reason.

An unusable file refuses with `settings_unreadable`. The error names the
path and reason. This covers unreadable files, non-regular files, malformed
JSON, non-object settings or `permissions`, a non-array `permissions.deny`,
and entries that are non-strings, empty or contain control characters.

| Limit | Effect |
| --- | --- |
| 1 MiB per settings file | Larger files are refused with `settings_unreadable`. |
| 256 rules per file | More rules are refused with `deny_rules_over_limit`. |
| 512 rules after expansion and deduplication | More rules are refused with `deny_rules_over_limit`. |
| 512 characters per rule | Longer rules are refused with `deny_rules_over_limit`. |
| 120 KiB generated `--settings` JSON | Larger arguments are refused with `deny_rules_over_limit` if deny rules are present, or `unsupported_command` otherwise. |
| 100 rules, 200 characters per rule | Dry-run display limits. `omitted` counts rules beyond the display count. |

Rules are passed as written. For `Read` and `Edit` patterns with exactly
one leading slash, delega also adds an absolute `//BASE/path` rule. BASE is
the user configuration directory for user settings, the working directory for
project and local settings, plus the source root for local settings from a
different repository root or main checkout. Relative patterns resolve against
the working directory. Duplicate rules keep their first occurrence.

The notes report which settings were excluded and how many rules were
applied, with paths and counts per file. The mapping shows `denyRules=N`.
Dry-run `deny_rules` gives `files`, `skipped`, `applied`, `user_deny_rules`
and `omitted`. Text output labels the rule preview “user deny rules” and the
dry-run `--settings` argument shows a count in place of rules. Results retain
paths and counts in [`applied.deny_rules`](api.md#applied), with
`applied.context.settings: "deny_rules"`.
See `crates/delega-core/src/provider/deny.rs`.

### Strict

`permissions.strict` refuses Claude Code `build` because command reads can
leave the requested directories. It also refuses `read` and `write` with
exact commands when no command guard is configured. Both use
`permissions_not_enforceable`. The CLI supplies its own executable for the
guard when it can resolve an absolute path.

### What cannot be enforced

- A working directory of `/` or the user's home grants access below it and
  adds a warning.
- `full` keeps sub-agents, MCP and background tasks disabled and notes this.
  Without `on_request: ask`, `write` and `build` omit `NotebookEdit`.
- Claude Code skips automatic `CLAUDE.md` and auto-memory loading and notes
  this. A worker can read repository instructions with its tools.
- Managed policy can override the launch settings.
  See [Managed settings](#managed-settings).

### Resume, ephemeral, effort

`session.resume` is `--resume <id>`. `session.ephemeral` is
`--no-session-persistence`, and the result then has no `session_id`. `effort`
is `--effort`. `model` is `--model`. The request has no turn or budget fields, so
`--max-turns` and `--max-budget-usd` are not set; the time limit is the bound,
and both can be passed as `native_args`.

### Stream

Claude Code emits one JSON event per line. The first root `result` supplies
the answer, outcome, denials and usage. A result with no answer text falls back
to the last root assistant message and adds a note. Sub-agent events do not
supply answers, session IDs or terminal results.

| Result | Source |
| --- | --- |
| `usage` | Per-run `usage` tokens. Input adds cache creation tokens and `cached_tokens` counts cache reads. |
| `usage.cost_usd` | Sum of per-model `model_usage_since_last_result.costUSD` when all supplied model costs are valid. Otherwise a fresh run uses `total_cost_usd`; a resumed run omits cost. |
| `session_usage` | Session `total_cost_usd` and the token totals across `modelUsage`. |

Negative or non-finite usage values are ignored. Failed results show
`errors`, then `result` text if no errors, then the subtype if neither is
available. Structured provider failure fields take precedence over message
matching. See [Provider failure kinds](#provider-failure-kinds) and
`crates/delega-core/src/provider/claude.rs`.

### `native_args` allowlist

| Option | Value |
| --- | --- |
| `--max-turns` | a whole number of at least 1, up to 10 digits |
| `--max-budget-usd` | a decimal number, at most 10 characters |
| `--fallback-model` | a model-like name up to 200 characters: letters, digits, `. _ : / -`, starting with a letter or digit |

Both `--flag value` and `--flag=value` are accepted.

### Live asking

An `on_request: ask` run adds `--input-format stream-json --permission-prompt-tool stdio`
and uses `--permission-mode manual --permission-prompts host`. The stdio flag
enables `can_use_tool` requests.

See [MCP](api.md#mcp) for `respond` and
[Permission interactions](supervisor.md#permission-interactions) for timeout
and teardown behavior. The nine built-in tools are available for requests. Mode grants still apply
automatically. Explicit denies, the exact-command guard and build's sandbox
still apply. In read and write mode, delega adds a Bash ask rule to prevent
Claude Code from automatically approving read-only shell commands. When an
exact-command guard is active, its exact grants and denials apply and delega
leaves out the Bash ask rule. Build also leaves it out so sandboxed commands
can run automatically. Read, write and build ask before WebFetch and WebSearch.
Use `Edit(path)` for path-specific file edit denies, including
writes ([live probe](#verified-against-the-real-cli)).

The host sends the assembled prompt as one JSON user message, then answers
permission requests over stdin. stdin closes on the first result or during
teardown. Each run can have up to 32 pending requests and process at most 256.
A permission frame over 256 KiB with a valid request is denied individually.
A stream frame over 4 MiB or a request beyond the lifetime limit ends the run
as `failed` with `wait_failed` and the message
`Claude Code permission protocol failed`. Pending overflow is denied.
Non-JSON stdout lines are counted in diagnostics. See `crates/delega-core/src/provider/ask.rs`.

## Codex CLI

Command shape, in order:

```text
codex --disable plugins --disable apps --disable hooks --disable multi_agent
  -c approval_policy="never"
  -c sandbox_mode="<read-only|workspace-write|danger-full-access>"
  -c allow_login_shell=false
  [-c sandbox_workspace_write.network_access=false
   -c sandbox_workspace_write.exclude_slash_tmp=<true|false>
   -c sandbox_workspace_write.exclude_tmpdir_env_var=<true|false>]
                                    (write: true; build: false)
  [-c model_reasoning_effort="<low|medium|high>"]
  -c developer_instructions="<text>"
  -c mcp_servers={...every listed server disabled...}
  [native_args]
  exec [resume <id>] --ignore-user-config --json --skip-git-repo-check
  [--ephemeral] [--model <m>] -
```

The adapter sets no environment variables.

### Login shell

Codex gets `-c allow_login_shell=false` in all modes. Commands run without
loading the user's shell profile, so profile exports need explicit passing
through the [environment policy](#environment-policy). See the
[verified environment counts](#verified-against-the-real-cli) and
`crates/delega-core/src/provider/codex.rs`.

### Permission mapping

| Mode | Sandbox | Approval | Notes |
| --- | --- | --- | --- |
| `read` | `read-only` | `never` | Writes are blocked. |
| `write` | `workspace-write`, network off, `/tmp` and `$TMPDIR` excluded | `never` | Writes limited to the working directory. |
| `build` | `workspace-write`, network off, `/tmp` and `$TMPDIR` included | `never` | Writes limited to the working directory and the temporary directories. |
| `full` | `danger-full-access` | `never` | No sandbox. |

`approval_policy="never"` means nothing is ever asked: an action the sandbox
does not allow fails.

### Wider than requested

Codex `read`, `write` and `build` dry runs and results that carry plan
notes say `wider than requested`. Commands can read anywhere the user can.
In `read` and `write`, any command permitted by the sandbox can run.
`permissions.commands` and `permissions.directories` add no grants.
`permissions.strict` (`--strict`) refuses these modes with
`permissions_not_enforceable` and names the gaps. Use Claude Code when
exact commands and scoped reads are required.

In `build`, directories under the working directory, `/tmp` or `$TMPDIR`
can be writable even when listed as read-only. Git metadata is protected,
so `git add` and `git commit` failed in both a checkout and a worktree during
live verification. A build note tells the caller to commit the changes.

### What cannot be enforced

- Only a command marked `declined` appears in `permissions.denied`.
  Other sandbox blocks appear as failed commands. A run note explains this.
- Codex may load its own `AGENTS.md` files despite `--ignore-user-config`.
- Managed policy can override the launch settings.
  See [Managed settings](#managed-settings).

### Stream

`thread.started.thread_id` supplies the session ID. A non-commentary agent
message supplies the answer. `turn.completed` and `turn.failed` supply the
terminal outcome and any usage. An `error` event fails the run if no terminal
event follows.

Codex reports thread totals in `session_usage`, including input, output,
cached input and reasoning output tokens. A fresh run also reports the totals
in `usage`. A resumed run leaves `usage` empty. Before a terminal event
with usage, neither field is available.

Failure classification uses structured `codex_error_info`, then structured
JSON error text, then message matching. An event with a code and no message
can surface as `Codex reported <name>`. See
[Provider failure kinds](#provider-failure-kinds) and
`crates/delega-core/src/provider/codex.rs`.

### `native_args` allowlist

| Option | Value |
| --- | --- |
| `-c` or `--config` | `model_verbosity=low\|medium\|high` or `model_reasoning_summary=auto\|concise\|detailed\|none` |

Any other `-c` key or value is refused, because configuration keys can change
the sandbox, approval policy or working directory.

## Rejected `native_args`

Anything not on the allowlist is `refused` before start with kind
`native_arg_rejected`. The message names the option (up to 40 characters) and position and omits the value. Options known to change permissions, sandboxing, the working directory,
the configuration location, the session or the output format get a message saying
so (for example `--permission-mode`, `--add-dir`, `--settings`, `--output-format`,
`--sandbox`, `--full-auto`, `--cd`, `--json`, `--profile`). A value without an
option before it is refused.

## Environment policy

delega starts with an allowlist from the parent environment, then applies
caller assignments and adapter settings.

| Group | Names or source |
| --- | --- |
| Base | `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LANGUAGE`, `TERM`, `TMPDIR`, `TZ`, `LC_ALL`, `LC_COLLATE`, `LC_CTYPE`, `LC_MESSAGES`, `LC_MONETARY`, `LC_NUMERIC`, `LC_TIME`, `LC_ADDRESS`, `LC_IDENTIFICATION`, `LC_MEASUREMENT`, `LC_NAME`, `LC_PAPER`, `LC_TELEPHONE`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME`, `NO_COLOR`, `CI`. |
| Network | `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY` in upper and lower case, `SSL_CERT_FILE`, `SSL_CERT_DIR`, `NODE_EXTRA_CA_CERTS`. |
| Provider | Claude: `CLAUDE_*`, `ANTHROPIC_*`. Codex: `CODEX_*`, `OPENAI_*`. Calling-session names are excluded. |
| Named by caller | `env_pass`, populated by `[env] pass` and repeatable `--env NAME` (`-e`). Missing names appear as `named_not_set` in dry runs. Already-allowed names keep their group. |
| Set by caller | `env_set`, populated by `[env.set]` and `--env NAME=VALUE`. Flags override config; assignments override values passed from the parent. |
| Set by adapter | The four Claude variables listed under [Claude Code](#claude-code). Attempts to name or set these are refused with `forbidden_env_name`. |

Caller names accept ASCII letters, digits and underscores and start with a
letter or underscore. `invalid_env_name` omits the invalid name in case it
contains a value. `forbidden_env_name` covers calling-session and `DELEGA_*`
names. You can explicitly pass variables belonging to the other provider.
Secret-looking names added or set by the caller get a note. Use `--env NAME`
for secrets to keep values out of shell history and the process table.
See [Configuration file](api.md#configuration-file).

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

`dry_run().env_names` lists the passed names. `dry_run().env` (`EnvReport`)
groups them as `base`, `network`, `provider`, `named`, `set_by_caller` and
`set_by_delega`, and gives `named_not_set` and `withheld_count`. Withheld
names, values, prompt and instructions are omitted. `ProviderOptions` and
`ResolvedEnv` debug output show environment names only.

The run note gives withheld and passed counts, including values added by the
caller and adapter in the passed count. See [`applied.env`](api.md#applied)
for counts by reason and caller-supplied names. Provider output can contain
values ([redaction rules](#text-from-the-provider)).
See `crates/delega-core/src/provider/env.rs`.

## Verified against the real CLI

### 2026-11-18, Claude Code 2.1.328 and codex-cli 0.170.0

- Scoped `Read(//dir/**)` scoped reads under `dontAsk`.
- `Grep` and `Glob` followed scoped Read rules when omitted from the allow
  list. A bare `Grep` allow rule let it read outside the working directory.
- `--add-dir` made a directory readable and unwritable by tools. Sandboxed
  commands could write there in the 2026-11-22 probe below.
- The command hook ran and its denial was honoured.
- Both version outputs parsed.
- Claude `--effort low` and the Codex effort key were accepted.
- Claude `--no-session-persistence` and Codex `--ephemeral` were accepted.
- Both providers exited 0 after success.
- Codex `workspace-write` blocked writes outside the working directory.
- Both providers resumed sessions.
- Claude inline `permissions.deny` took effect with empty setting sources.
  A project rule blocked Read, hid a file from Grep and overrode an allowed
  command. Inline `./x`, `x` and `**/x` matched at multiple depths from the
  working directory. The added absolute form of a one-slash rule was untested.
- Codex command `env` listed 28 names with login shells disabled, none from
  profiles or secret-looking. With login shells enabled it listed 63, including
  10 secret-looking names. delega passed 9.

### Claude Code 2.1.329 and codex-cli 0.170.0, dates unrecorded

- Both providers reported the requested session ID on a follow-up (#114).
- Claude resume with an unknown ID exited 1 with an `error_during_execution`
  result naming the requested ID and `No conversation found with session ID`.
  delega returned `session_not_found`.
- Codex resume with an unknown ID exited 1 with empty stdout and
  `no rollout found for thread id` (code -32600) on stderr.
  delega returned `session_not_found`.
- Claude with an unknown model reported `model_not_found` and HTTP 404 in an
  assistant event, then emitted a result with subtype `success`,
  `is_error: true` and no `errors`, and exited 1. delega returned
  `provider_model_unavailable` (#116).
- Claude with an invalid key reported `authentication_failed` and HTTP 401 in
  the same event shape. delega returned `provider_auth`. The executable listed
  these assistant `error` values (#116): `authentication_failed`,
  `oauth_org_not_allowed`, `account_on_hold`, `verification_required`,
  `billing_error`, `rate_limit`, `overloaded`, `invalid_request`,
  `model_not_found`, `server_error`, `unknown`, `max_output_tokens` and
  `cloud_credential_error`. The two probes exercised
  `model_not_found` and `authentication_failed`.
- Across a fresh run and two resumes, Claude reported per-run output tokens
  (601, 4, 3) and cumulative cost (USD 0.0283, 0.0374, 0.0388). A separate
  sample reported fresh and resumed session cost of USD 0.01142 and 0.0173514
  and per-run cost of USD 0.01142 and 0.0059314, respectively. The resumed
  result's per-model session and per-run output counts were 205 and 4.
  Codex reported cumulative input, output and cached input counts in
  `turn.completed.usage`: (16,229 / 603 / 13,440),
  (33,074 / 608 / 26,880), and (49,937 / 613 / 43,648) (#115).
- Codex with an unknown model on a ChatGPT account or an invalid API key
  exited 1 with text-only `error` and `turn.failed` messages. The invalid
  key produced ten reconnect messages with HTTP 401. delega returned
  `provider_model_unavailable` and `provider_auth`, respectively (#116).

### 2026-11-22, Claude Code 2.1.330 and codex-cli 0.171.0 (#98)

- Raw Claude with delega's launch flags plus the [Build](#build) sandbox
  settings ran `cargo test` under `dontAsk` without a Bash allow rule. Its
  per-user `TMPDIR` was writable, the rest of `/tmp`
  and `$HOME` were blocked, and the network was denied. An `--add-dir`
  directory was writable unless covered by `filesystem.denyWrite`.
- Raw Codex with `network_access=false` and both temporary-directory exclusions
  set to `true` failed `cargo test` with `PermissionDenied`. With both exclusions
  set to `false`, tests passed, `$TMPDIR` and `/tmp` were writable, and
  `$HOME` and the network were blocked. The network result also matches the
  sandbox default.
- With the branch binary for #98 on macOS (Claude model haiku), both build
  workers passed `cargo test` and were denied writes to `$HOME` and network access.
- The Codex build worker could write a listed directory under `/tmp`.
  Git index locking and writing `.git/hooks/x` failed, blocking commits in
  both a regular checkout and a linked worktree.
- The Claude build worker could write to its per-user `TMPDIR`, including
  the supervising session's scratch directory, and could stage and commit
  files from a worktree. Writing main-checkout files or hooks, locking
  `.git/config`, and writing a listed directory covered by `denyWrite`
  failed. Claude recorded a denial for `git -c user.name=x -c user.email=y
  commit`; the same commit without `-c` succeeded.
- With the same Claude branch binary, a `denyWrite` rule for a listed parent
  directory also denied writes inside the working directory.
- Raw Claude with a scoped Read rule and no `--add-dir` read the extra
  directory with the Read tool and blocked command writes there. The working
  directory stayed writable and Read on `/etc/hosts` was denied.

### Claude Code 2.1.330, date unrecorded

- In ask mode the init event reported `default`. The stdio flag enabled
  `can_use_tool` requests. A path-specific `Write(path)` deny did not
  apply to the probed write (#157).

### 2026-11-22, Claude Code 2.1.330 (#157)

- With only `Edit(//<cwd>/**)` allowed under `dontAsk` (model haiku, the branch
  binary for #157), the Write tool created a file in the working directory,
  the Edit tool changed it, and a Write outside the working directory was
  denied and listed in `permissions.denied`. The CLI reports a `Write(path)`
  allow rule as never matched (#157).

## Unverified against the real CLI

Dates and versions are unrecorded unless indicated.

| Open question | Current mapping or evidence |
| --- | --- |
| Does Claude `full` work without `bypassPermissions`? | Grants the nine built-in tools under `dontAsk`. |
| Does Codex honour the explicit `network_access=false` override? | 2026-11-22, codex-cli 0.171.0 blocked network access, which is also the workspace-write default. |
| Does Codex drop explicitly passed secret-looking variables from commands? | `--env` passes them to Codex. Command inheritance is untested. |
| Does Codex emit declined commands as `item.completed` with `status: "declined"`? | The parser uses that shape for `permissions.denied`. |
| Does Codex accept the allowlisted `model_verbosity` and `model_reasoning_summary` values? | Both are allowed as native config overrides. |
| Are native schema options needed? | delega uses the prompt and validates the answer. |
| Do Claude and Codex accept all three effort values? | `low` was verified on 2026-11-18 with Claude Code 2.1.328 and codex-cli 0.170.0. |
| Do the Claude instruction and memory switches suppress automatic loading? | Claude Code 2.1.329 listed no `CLAUDE.md`, `AGENTS.md` or auto-memory file in the probed context (#86, #106; date unrecorded). |
| Can a denial cause a Claude terminal failure? | The attribution rule can return `refused` with `permission_denied`. |
| Does a denial-only Claude failure name the denied tool or indicate permission trouble? | The parser looks for that text when present. |
| Does Claude emit the recorded usage-limit events? | Claude Code 2.1.314 was previously observed emitting `usage_limit_reached` with HTTP 429 and a failed result, with a possible rejected `rate_limit_event`. delega classifies usage exhaustion ahead of capacity unless the rejection is covered by overage. Date unrecorded. |
| What shapes does Claude emit for capacity, billing, other account errors and context overflow? | The parser recognises statuses 429, 503 and 529, assistant error codes and the result prefix `Prompt is too long`. |
| What shapes does Codex emit for usage, capacity and context failures? | Prior usage-limit text and structured `usage_limit_exceeded`, capacity messages and `context_window_exceeded` are recognised. |
| Does Claude emit `error_max_turns` and `error_max_budget_usd` results? | They map to `provider_turn_limit` and `provider_budget_limit`. |

## Managed settings

Managed policy can override launch options without a run note. Consult
[Doctor](api.md#doctor) for report fields and the provider's own references
for the effect of a key. Doctor reports fixed paths, counts and recognised
keys by category. Values, user names and drop-in file names are excluded.

Claude planning refuses with `managed_mcp_exclusive` when it can read
`managed-mcp.json` as any JSON value, including `[]`, `null` or an empty
server map. A zero-byte file does not trigger this refusal. Doctor reports a
blocking problem for the same condition. See the
[Claude Code managed MCP reference](https://code.claude.com/docs/en/managed-mcp#servers-passed-with---mcp-config-or---strict-mcp-config).

**Sources looked for.** Windows is out of scope.

| Provider | Source | Where | Reported |
| --- | --- | --- | --- |
| Claude Code | `managed-settings.json` | `/Library/Application Support/ClaudeCode` on macOS, `/etc/claude-code` on Linux | The keys below that are present. |
| Claude Code | `managed-settings.d/*.json`, hidden files left out | The same directory | The directory once, with the number of files, how many could not be read, and the keys below from all of them together. No file is named. |
| Claude Code | `managed-mcp.json` | The same directory | Its presence and read state. A readable, parsing file also appears as a problem. Server names are not reported. |
| Claude Code | Managed preferences domain `com.anthropic.claudecode` | macOS: `/Library/Managed Preferences/com.anthropic.claudecode.plist` and the same file in each user's directory there | The system file as present, not inspected; the users' files as one source with a count. No user is named. |
| Codex | `requirements.toml` | `/etc/codex` on macOS and Linux | The top-level keys Codex reads, by name, and a count of the others. |
| Codex | `managed_config.toml` (legacy) | `/etc/codex` on macOS and Linux | The top-level keys listed below, by name, and a count of the others. |
| Codex | Managed preferences domain `com.openai.codex` (`config_toml_base64`, `requirements_toml_base64`) | macOS: `/Library/Managed Preferences/com.openai.codex.plist` and the same file in each user's directory there | As for Claude Code. |

| Read state or limit | What doctor reports |
| --- | --- |
| `read` | A parsed source. Empty Claude settings and drop-ins count as empty objects. |
| `unreadable` | An unreadable, non-regular or invalid source, or a file over 4 MiB. Claude settings require an object root. |
| `not_inspected` | Managed preferences are present. Doctor does not inspect their contents. |
| `partly_read` | Some drop-ins were unreadable or a scan limit was reached. A changing file may also be reported this way without keys. |
| 64 drop-in files, 4,096 directory entries, 4 MiB aggregate | Reaching a drop-in scan limit reports `limit_reached`. |
| 4,096 per-user preferences entries | Reaching the listing limit reports `partly_read` with `limit_reached`. |

**Claude Code keys.** Doctor lists these keys when they are present, whatever
their value, with the category shown. A key that is not in this table is not
listed. Nested keys are given as dotted names. The category places a key; what
the key does is in Claude Code's
[settings reference](https://code.claude.com/docs/en/settings-reference).

| Category | Keys |
| --- | --- |
| `permissions` | `allowManagedPermissionRulesOnly`, `permissions.allow`, `permissions.deny`, `permissions.ask`, `permissions.additionalDirectories`, `permissions.blockReadsOutsideWorkingDirectories`, `permissions.blockWritesOutsideWorkingDirectories` |
| `hooks` | `allowManagedHooksOnly`, `disableAllHooks` |
| `plugins` | `strictPluginOnlyCustomization` |
| `mcp` | `disableSideloadFlags`, `allowManagedMcpServersOnly`, `allowedMcpServers`, `deniedMcpServers` |
| `sandbox` | `sandbox.enabled`, `sandbox.autoAllowBashIfSandboxed`, `sandbox.allowUnsandboxedCommands`, `sandbox.failIfUnavailable`, `sandbox.excludedCommands`, `sandbox.filesystem.allowWrite`, `sandbox.network.allowedDomains`, `sandbox.filesystem.allowManagedReadPathsOnly`, `sandbox.network.allowManagedDomainsOnly` |
| `authentication` | `forceLoginMethod`, `forceLoginOrgUUID`, `enforceAnthropicSignIn`, `allowedProviders`, `apiKeyHelper` |
| `environment` | `env` (the variable names in it are not listed) |
| `model` | `availableModels`, `deniedModels`, `modelOverrides` |
| `version` | `requiredMinimumVersion`, `requiredMaximumVersion` |
| `policy` | `policyHelper` |

**Codex keys.** A top-level key is named only when doctor knows it. Any other
key is counted in `other_keys` and not named, and the text report says how many
there are. What a key does is in Codex's source: `ConfigRequirementsToml` in
`codex-rs/config/src/config_requirements.rs` and the layering described in
`codex-rs/config/src/loader/README.md`, both in
<https://github.com/openai/codex>.

| Category | File | Keys named |
| --- | --- | --- |
| `codex_requirements` | `requirements.toml` | The fields of `ConfigRequirementsToml` and the aliases it accepts, such as `allowed_approval_policies`, `allowed_sandbox_modes`, `allowed_permission_profiles`, `default_permissions`, `mcp_servers`, `allow_login_shell` and `rules`. |
| `codex_config` | `managed_config.toml` | The settings delega also passes with `-c` (`approval_policy`, `sandbox_mode`, `allow_login_shell`, `sandbox_workspace_write`, `developer_instructions`, `model_reasoning_effort`, `mcp_servers`) and `approvals_reviewer`. |

Both files are read by Codex whatever is on the command line:
`--ignore-user-config` leaves out the user's `config.toml` only.

**What doctor cannot see.**

- Settings a provider fetches from its server: Claude Code's server-managed
  settings from the claude.ai admin console or a gateway, and Codex's
  cloud-managed configuration and requirements. Nothing local says they exist.
- The content of managed preferences, and what a `policyHelper` returns.
- Policy an embedding host passes to Claude Code.
- `/etc/codex/config.toml`, the system configuration layer of Codex. Codex's
  loader documents it as the lowest layer, below the `-c` settings, and it is
  not reported.
- Windows locations (the registry and `%ProgramData%`).

So an empty report does not prove that no managed policy applies. In Claude
Code, `/status` names the managed sources in force.

See `crates/delega-core/src/provider/managed.rs`.

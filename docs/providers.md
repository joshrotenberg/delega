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
`env_pass`, `env_set`, `parent_env`, `run_id`, `worker`, `max_output_bytes` and
`stop_grace`. Its `Debug` output lists environment variable names only.

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
`provider_not_found`, `unsupported_provider_version` (all refused before start);
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
| `read` | `Read,Glob,Grep` | `Read(//CWD/**)`, one `Read(//DIR/**)` per directory, `Glob`, `Grep` | hooks off |
| `write` | `Read,Glob,Grep,Edit,Write` | the `read` rules plus `Edit(//CWD/**)`, `Write(//CWD/**)` | hooks off |
| `full` | all nine built-in tools: `Read,Glob,Grep,Bash,Edit,Write,NotebookEdit,WebFetch,WebSearch` | the same nine, unscoped | hooks off |

### Commands and directories

- `permissions.commands` (`read` and `write`): `Bash` is added to `--tools`, one
  `Bash(<command>)` rule per command is added to `--allowedTools`, and a blocking
  `PreToolUse` hook is installed in `--settings`. The hook allows a `Bash` call
  only when its command equals a granted command byte for byte, and denies a call
  that asks to run in the background or to disable the provider's sandbox. In
  `full`, commands are not listed: every command is allowed.
- The hook is a program the caller supplies in `ProviderOptions::hook`
  (`HookCommand { program, args }`). `provider::hook::run` is the entry point it
  calls; the `delega` binary will expose it as a subcommand. Without a hook,
  `permissions.commands` is not granted at all and a note says so. Claude Code
  must be 2.1.325 or later for the hook interface.
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

- `Glob` and `Grep` are granted without a path scope. Whether Claude Code
  confines them to the working directory and listed directories is the provider's
  behaviour, not delega's. A note says so on every `read` and `write` run.
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

The provider gets the parent environment minus three groups of variables, then
`env_set`, then the adapter's own variables.

1. **The other provider's.** A Claude Code run loses `CODEX_*` and `OPENAI_*`,
   and the host session markers `CLAUDE_CODE_MESSAGING_SOCKET`,
   `CLAUDE_CODE_MESSAGING_TOKEN` and `CLAUDE_CODE_BRIDGE_SESSION_ID`. A Codex run
   loses `CLAUDECODE`, `CLAUDE_*` and `ANTHROPIC_*`. The earlier project did the
   same.
2. **Names that look like secrets**, unless they belong to the provider being
   run (`ANTHROPIC_*`, `CLAUDE_*`, `CLAUDECODE` for Claude Code; `OPENAI_*`,
   `CODEX_*` for Codex). A name matches when, in upper case, it contains
   `SECRET`, `TOKEN`, `PASSWORD`, `PASSWD`, `CREDENTIAL`, `PRIVATE_KEY` or
   `ACCESS_KEY`, ends in `_KEY`, `_PAT` or `APIKEY`, or is `SSH_AUTH_SOCK`.
   The earlier project passed every other variable. This rule is added because a
   worker can print its environment. It is a heuristic; a secret under an
   ordinary name is passed.
3. **delega's own settings**: `DELEGA_*`.

`ProviderOptions::env_pass` names variables to pass although 1 to 3 would remove
them. `env_set` sets variables after the policy runs. The adapter's variables
(`CLAUDE_CODE_DISABLE_BACKGROUND_TASKS`, `DELEGA_EXACT_BASH_GRANTS`) cannot be overridden. A variable whose name is not valid UTF-8 is dropped.

Only names are reported: `dry_run().env_names`, `ProviderOptions`'s `Debug` and
the `ResolvedEnv` type show names. The prompt, the instructions and variable
values are not in a dry run. In a result, the only text that can carry a value is
text the provider wrote (an error message or the standard error excerpt), which
is redacted as described under [Text from the provider](#text-from-the-provider),
and the answer, which is the provider's own output.

## Unverified against the real CLI

The fake providers in the tests copy the stream formats of the earlier
project's fakes. The fake Claude Code also honours its tool, rule and hook
arguments, and the fake Codex its sandbox flag, so that the tests show
enforcement through `run`; that proves the plumbing against the behaviour
assumed here, not the real CLIs. Nothing here was run against the real `claude` or `codex`. The choices below are not
confirmed and were made conservatively. A live run per provider should settle
each.

| Assumption | Conservative choice |
| --- | --- |
| Claude Code accepts `Read(//abs/dir/**)` as an allow rule and then allows reading only there under `dontAsk` | The earlier project used this rule form for deny rules. If allow rules do not work, reads fail closed. |
| Claude Code applies Read scoping to `Glob` and `Grep` | Not relied on: both are granted unscoped and a note says so. |
| `--add-dir` does not make an extra directory editable in `write` mode | No `Edit` or `Write` rule names it. |
| `--no-session-persistence` exists with `-p` | Used for `session.ephemeral`. An unknown flag would fail the run. |
| `claude --version` prints the version first, such as `2.1.400 (Claude Code)` | An unparseable version refuses exact command grants. |
| The hook settings accept an `args` list next to `command` | Taken from the earlier project's settings, which gate on 2.1.325. |
| `full` for Claude Code needs no `--permission-mode bypassPermissions` | The nine built-in tools are allowed under `dontAsk`, the mechanism the earlier project already used. |
| Codex honours `sandbox_workspace_write.network_access`, `exclude_slash_tmp` and `exclude_tmpdir_env_var` given with `-c` | They only narrow the sandbox. An unknown key would fail the run. |
| `codex exec --ephemeral` exists | Used for `session.ephemeral`. An unknown flag would fail the run. |
| A declined command is `item.completed` with `status: "declined"` | Only those are listed in `permissions.denied`, and a note says other blocks are not. |
| `model_verbosity` and `model_reasoning_summary` are accepted config keys with the listed values | They are the only `-c` keys allowed. |
| Both providers exit 0 after a successful result | A non-zero exit after success is `failed`. |
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

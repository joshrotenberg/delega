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
2. `DELEGA_CLAUDE_BIN` or `DELEGA_CODEX_BIN` in the parent environment (empty
   counts as unset).
3. `ProviderOptions::claude_executable` or `codex_executable`, which `delega`
   fills from `[providers.claude] executable` or `[providers.codex] executable`
   in the user-level [configuration file](api.md#configuration-file). Doctor
   reports it as `config`.
4. `claude` or `codex` on the absolute entries of `PATH`. Relative and empty
   entries are skipped, so a program in the working directory is never picked up.

A path from 1, 2 or 3 must be absolute and an executable file. Otherwise the run is
`refused` with kind `provider_not_found`, and the message names the variable to
set. The path used is `provenance.provider_executable`.

## Diagnostics

`delega doctor` ([api.md](api.md#doctor)) runs this lookup and the `--version`
probe of step 2 below, with the same environment and time limit, for both
providers, and reads the Claude Code settings with the reader described under
[Deny rules](#deny-rules), without starting a worker. It reports the executable,
the version and whether the command hook interface is available, counts of the
environment variables and deny rules a run would get, and the problems that
would refuse a run, each with a stable kind (the kind a refused run has, where
there is one), plus the process inspection check. A failed or timed-out Claude probe appears in doctor as
`version_probe_failed` or `version_probe_timed_out`, while a run with an allowed
command refuses the same condition as `unsupported_provider_version`. It never
prints a value or a rule.

It also names the managed (administrator) settings it finds for each provider,
as a notice that changes no verdict ([Managed settings](#managed-settings)).

## Run sequence

1. Validate the request (`Request::validate`), the `native_args`, and the
   `answer_schema` (it must compile). Resolve `cwd` and `permissions.directories`
   (each must exist). Look up the executable. Any failure is `refused` before start.
2. Run `<executable> --version` (5 s). The first line is
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
| `failed` | Provider error event or result (`provider_error`, or `provider_turn_limit` or `provider_budget_limit` when Claude Code names a limit, or, when the failure says so, `provider_usage_limit`, `provider_auth`, `provider_billing`, `provider_capacity`, `provider_model_unavailable` or `provider_context_exhausted`, see [Provider failure kinds](#provider-failure-kinds)), non-zero exit (`provider_exit`, with a bounded, redacted excerpt of standard error, see [Text from the provider](#text-from-the-provider)), success followed by a non-zero exit, no terminal event and a clean exit (`no_final_message`), output that was not events (`malformed_output`), success with no answer text, `answer_not_json`, a resumed run that would otherwise have completed but whose stream reported another session than `session.resume` (`session_mismatch`; any other outcome stands with a note), a resumed run whose session the provider could not find (`session_not_found`), a failed preparation step, a failure to start. |
| `timed_out` | The time limit passed. `text` holds any partial answer. |
| `interrupted` | `interrupt()` was called, or the handle or future was dropped. |
| `refused` before start | See steps 1 and 2 above. `timing.started_at` is absent. |
| `refused` after start | The run stopped because of a denial, by the rule below. Kind `permission_denied`. |

**When a run is `refused` after start.** Only when all of these hold: the
provider's first terminal event is a failure; that failure is not a limit the
provider names (`provider_turn_limit`, `provider_budget_limit`) and has none of
the [provider failure kinds](#provider-failure-kinds); at least one request was denied; and nothing reported an
error separately and nothing went on after the last denial.

- Claude Code: the denials arrive in the failed result event. The run counts as
  stopped by the denial when no `error` event and no `api_retry` event was seen;
  when no assistant message or tool result came after the result of the last
  denied call; and when every entry in the result's `errors` names a denied tool
  or speaks of permission or denial (a result with no `errors` is judged by its
  `result` text the same way, and a result with neither counts as about the
  denial, as before). An entry or text such as "API connection lost" makes the
  run `failed`.
- Codex: the last item before the failure must be the `declined` command. Any
  later item (an agent message, another command, a `file_change`) or an `error`
  event ends the attribution. A `turn.failed` with one of the provider failure
  kinds (for example a usage limit) is `failed`.

Every other failure with a denial is `failed` with the denial listed in
`permissions.denied`. `provider_exit_code` is in the error of every `failed` and
refused-after-start result from a process that exited. These attributions are
heuristics over events whose exact shape is unverified (see the table at the
end).

The first terminal event decides. A later terminal event is ignored and a note
says a second one was seen, so a failure followed by a success is not a
completion. Output bytes that are not valid UTF-8 are decoded with replacement
characters and a note says how many lines were affected.

**Resumed sessions.** When `session.resume` is set, every session id the stream
reports is compared with it; any other id is a mismatch, including when the
stream reports the requested id and another one. The whole stream counts: the
first terminal event still decides the answer, but ids reported after it are
compared too. Ids in Claude Code sub-agent events (those with
`parent_tool_use_id`) are not taken, as their text is not. `session_mismatch`
replaces only an outcome that would otherwise be `completed`: such a run is
`failed` with kind `session_mismatch`, the message names both ids, `text` keeps
the answer as partial text, and `session_id` is the latest id the provider
reported up to its first terminal event. Every other outcome stands (for example `timed_out`, `interrupted`,
`answer_not_json` or `provider_error`), and a note names both ids. Both the
message and the note are redacted like other provider text (see
[Text from the provider](#text-from-the-provider)). A run that reports no
session id is not a mismatch. An `ephemeral` run gets no session checks at all.
A run that is neither resumed nor `ephemeral` and whose stream reports more than
one session id gets a note naming them: at most the first 8 distinct ids, with
the number of further distinct ids that are not listed.

The session checks cover the captured output only. Standard output is kept from
its start up to `max_output_bytes`; when it was cut at that limit (the run gets
a note saying so), session ids past the limit are not checked.

A resumed run that failed because the provider could not find the session has
kind `session_not_found` instead of `provider_error` or `provider_exit`, with
the same message: for Claude Code, a failed result whose error text starts
`No conversation found with session ID`; for Codex, a run with kind
`provider_exit`, or `provider_error` after a JSON `error` event, that exited
with code 1, reported no thread and no terminal event, whose standard output
was not cut at the capture limit, and whose captured standard error (its last 64 KiB) contains `no rollout found for thread id`
anywhere. Neither is matched when
`session.resume` is not set.

### Error kinds

`invalid_request`, `invalid_answer_schema`, `native_arg_rejected`,
`cwd_unusable`, `directory_unusable`, `unsupported_command`,
`permissions_not_enforceable`, `provider_not_found`, `unsupported_provider_version`, `invalid_env_name`,
`forbidden_env_name`, `settings_unreadable`, `deny_rules_over_limit` (all
refused before start);
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

Only failed runs are classified; a success is never changed. `session_not_found`
outranks every kind here (a Claude Code result for a missing session may also
carry status 404 and `not_found_error`), and `provider_turn_limit` and
`provider_budget_limit` keep their precedence. After
them, a usage limit outranks the rest, because the same failure also carries a
rate limit and HTTP 429; then billing, auth, model, context and capacity, in
that order. A failure that matches none keeps `provider_error`. How each
provider's events are read is in its Stream section ([Claude Code](#stream),
[Codex](#stream-1)). The message is the provider's, redacted as below.

### Text from the provider

Provider standard error and provider error messages can echo what the provider
was given. Standard error is captured up to its last 64 KiB. All of the
captured standard error is redacted before the excerpt is cut from it: its last
8 KiB, joined to one line, and its last 300 characters. The excerpt's own cuts
come after redaction, so they cannot split a secret. When the 300-character cut
removed text, the excerpt starts with `... ` (three dots and a space, so the
marker never joins a word). After both cuts and the marker, the finished excerpt
is redacted once more, so neither cut can leave a newly key-shaped string (such
as the end of `xsk-ant-...` without its `x`) unredacted.

When standard error was longer than its 64 KiB capture, the start of the
capture is a cut made before redaction, and it can fall inside a secret: part of
a key, a token whose `Bearer` was not captured, a private key whose `BEGIN` line
was not captured, or half of a multi-byte character. The redactor cannot
recognise what is left of it. Two things keep that text out of the excerpt, and
neither applies to a capture that was not cut:

- When the capture begins with the end of a value of a redacted variable (see
  below), at least 6 characters of it, that end is replaced with `[redacted]`,
  however long it is, in the same pass as the whole values, so a whole value
  that overlaps it is replaced with it. If the cut split a character, the
  bytes left of it (at most 3, each decoded as U+FFFD) are stepped over first
  and go with the value. Other U+FFFD characters are text like any other.
- The excerpt is taken only from redacted text after a guard at its start. The
  guard is 13664 bytes: redaction keeps the order of the text and lengthens it
  by at most 10 bytes for every 6 (a 6-character value becomes `[redacted]`,
  and a replacement is never searched again, see below),
  so the first 8 KiB of the decoded capture end up within the first
  8192 * 10 / 6, rounded up, plus 10 bytes of the redacted text. The guard is
  cut from text that is already redacted, so it cannot split a secret that the
  capture holds whole.

A cut capture can end up with no excerpt: redaction can remove most of it so
that nothing but whitespace is left after the guard, or text can be left after
the guard while the last 8 KiB are blank. In both cases the message says so in
place of the excerpt: `(standard error not shown: the capture was cut and
nothing was left to show after redaction and the cuts)`. A capture that was not
cut and has no text (a provider that wrote no standard error, or only
whitespace) gets neither an excerpt nor this note.

The remaining limit: a credential-shaped secret that the capture's start cuts
and of which more than 8 KiB was captured (a very long token or private key
body, not a redacted variable's value) can still show its tail.

A provider error message is kept whole and cut to its first 500 characters
after redaction. Two kinds of text are replaced with `[redacted]`:

- the value of every variable passed to the provider whose name looks like a
  secret or belongs to the provider, when the value is at least 6 characters;
- strings shaped like API keys (`sk-`, `sk-ant-`), forge tokens (`ghp_`, `gho_`,
  `ghu_`, `ghs_`, `ghr_`, `github_pat_`), Slack tokens, AWS access key ids, JSON
  web tokens, `Bearer` tokens and private key blocks.

The values and private key blocks are replaced in one pass over the text as
the provider wrote it. Every occurrence of every value is found, occurrences
that overlap included. A private key block goes from a `-----BEGIN ... PRIVATE
KEY-----` line through its end line, or to the end of the text when it has
none. An end line is `-----END `, a label ending in `PRIVATE KEY` on the same
line, and `-----`; a `PRIVATE KEY-----` anywhere else, a `BEGIN` line included,
ends nothing, so a `BEGIN` line with no key of its own takes the block after it
with it. Blocks are looked for twice: in the text as written, and in the text
with the values replaced, with what is found there mapped back. Both findings
count, because a value can lie on a block's markers and each way then misses a
shape the other finds: as written, a value that holds a whole end line inside a
key body ends the block there; with values replaced, a value that overlaps a
`BEGIN` marker has removed it. Stretches that overlap or touch are replaced
together by one `[redacted]`. The replacements are not searched for values
again, so a value that happens to match inside `[redacted]` cannot lengthen the
text. The key shapes and `Bearer` tokens are replaced after that, and only
shorten the text.

Taking both findings removes at least what each removes alone.

The remaining limit: private key blocks are found by their begin and end
markers. Known values that overlap or imitate a block's markers can make both
looks miss the block or end it too early, and text after that point can then
show.

Tokens are found inside words, so `token=ghp_...`, `key: "sk-ant-..."`,
`https://user:ghp_...@host/` and JSON values are caught; `=`, `:`, `/`, `@`,
quotes and commas separate tokens. The text after `Bearer` is removed whole. The
shapes are a heuristic; a credential in another form is not caught. Configured
secret values that overlap delega's own markers (`...`, `[redacted]`) are not
supported. The answer text itself is the provider's output and is not redacted. A denied
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

Environment set by the adapter: `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1`,
`CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` and `CLAUDE_CODE_DISABLE_CLAUDE_MDS=1`
(Claude Code loads no `CLAUDE.md` file and no auto memory into the worker's
context by itself, and its auto-memory feature is off; `--setting-sources ""`
alone keeps `CLAUDE.md` out but not the auto-memory index, measured with Claude
Code 2.1.329 on #86) and, when the command guard is installed, `DELEGA_EXACT_BASH_GRANTS` (a JSON array of
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
  at all and a note says so; with `permissions.strict` the run is refused before
  start with `permissions_not_enforceable` instead. Claude Code must be 2.1.325 or later for the hook
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

### Deny rules

A Claude worker starts with `--setting-sources ""`, so Claude Code loads none of
the user's or the project's settings. One part of them is carried over: the
`permissions.deny` rules. A rule the user wrote can only narrow a run, so delega
reads those rules itself at launch and adds them to the worker. The earlier
project (commit `8fa4645`) merged them for every worker and anchored only its
reviewer workers' rules; this layer passes rules as written (see Anchoring).

**Files, in order.** The directories come from the environment the worker will
receive (`ProviderOptions::parent_env`, `env_pass`, `env_set` and the base
variables), not from the calling process.

1. User settings: `settings.json` in `CLAUDE_CONFIG_DIR` when the worker's
   environment has it, otherwise `.claude/settings.json` under the worker's
   `HOME`. When the worker has neither, no user file is looked for and a note
   says so.
2. `.claude/settings.json` under the working directory, and only there, as
   Claude Code reads it.
3. `.claude/settings.local.json` under the working directory, then under the root
   of the git repository the working directory is inside (Claude Code reads it
   from the repository root as well as the start directory), then under the main
   checkout when that root is a linked worktree. The root is the nearest ancestor
   holding `.git`. `git` is not run. The working directory's own files are
   always read.

   *A root is skipped.* As Claude Code does, a root that is the user's home
   directory is skipped, and so is a root whose root directory, `.git` entry,
   `.claude` directory or settings file is not owned by the current user. This
   applies to the main checkout as to any other root. The file is not read, and a
   note and the dry run's `deny_rules.skipped` name its path and the reason.

   *Links.* In a repository root other than the start directory, the root's
   `.git` entry is never followed if it is a symbolic link: the root's file is
   skipped for that reason. A symbolic link at `.claude` or at the settings
   file is followed only when all three hold: the link is owned by the current
   user; the directory that holds it (the root for `.claude`, the `.claude`
   directory for the file) is owned by the current user and not writable by group
   or others; and the target, once opened, is owned by the current user (a
   directory for `.claude`, a regular file for the file). Otherwise the file is
   skipped and the note says which condition failed, and a link to nothing is
   skipped as such. The reason: another user can plant a link only in a directory
   they can write, so a link the user owns in a directory only the user can write
   was put there by the user, as a dotfiles manager does, while a link in a
   group-writable or world-writable tree (or one under `/tmp`) may be planted to
   make the user's own files read as that project's rules. Ownership is judged
   on the open handles, each directory opened from the one above, so nothing
   swapped in after a check is read. Ownership is judged before kind and before
   readability: an entry or link target that the current user does not own is
   always skipped with the ownership note, whatever it is (a pipe, a device, a
   directory with mode 000) and is never read (a link target is not opened). Kind and
   readability errors refuse only for what the user owns: a link to a pipe the
   user owns, like a pipe named as the file, is refused as not a regular file; a
   link to `/dev/zero` is skipped as a foreign target (when run as its owner,
   root, it is refused as not a regular file). A file or directory the user owns
   that exists but cannot be opened or searched (mode 000 or 600, an I/O error)
   refuses the
   run with `settings_unreadable`, as it does for the start directory; only a
   file that does not exist contributes nothing. This holds for a link to a
   file or directory the user owns but cannot open (a target with mode 000), and
   also for a link whose target cannot be examined at all: one into a directory
   the user cannot search, or one whose path runs through a regular file. The
   owner of such a target cannot be known, so the rule that what someone else
   owns is skipped does not cover it. The run is refused because the link is the
   user's own, in a directory only the user can write, so a broken target is the
   user's to fix, and skipping it would drop their rules without a word. The user's own settings file
   (item 1) and the working directory's files (items 2 and 3) may be symbolic
   links, as a dotfiles setup makes them; a link to a regular file is followed.

   *The main checkout.* For a worktree, the `.git` file's `gitdir:` line is
   followed only to a directory directly under a `worktrees` directory inside a
   directory called `.git` (a worktree may be named `worktrees`), and only when
   that directory's `gitdir` file names this worktree's `.git` file. A `.git`
   file planted in a tree such as an unpacked archive therefore cannot borrow
   another checkout's rules. When the main checkout cannot be identified (a
   `.git` file that is not owned by the user, that has no `gitdir:` line or names
   nothing, a `.git` that is a symbolic link or a git directory kept elsewhere,
   or a `gitdir` file that does not name this worktree) only the nearest root is
   read and a note says the main checkout could not be identified and why. A
   `.git` file that points elsewhere than a `worktrees` directory, as a
   submodule's does, is not a linked worktree and gets no note.

Home and configuration directories are resolved to their real paths, so paths in
notes are real paths. Managed (administrator) settings are not read for deny rules: Claude Code applies them itself
([Managed settings](#managed-settings)). A missing file contributes nothing, and an empty
file (or one with only whitespace) is a file with no rules. A UTF-8 byte order
mark is ignored.

**What is taken.** Only `permissions.deny`, a list of strings. Everything else in
a file is dropped when it is parsed and is in no argument, note, result or dry
run. That covers hooks, `env`, `apiKeyHelper`, plugins, skills, MCP servers,
instructions and the `allow` and `ask` lists.

**Refused before start.** A file that exists and cannot be used refuses the run,
with kind `settings_unreadable`, naming the file and the reason and never its
content: it cannot be inspected or read, it is not a regular file (a directory,
device or pipe is refused before it is opened, and the file is opened without
blocking and checked again), it is larger than
`SETTINGS_FILE_BYTES` (1 MiB), it is not a JSON object, `permissions` is not an
object, `permissions.deny` is not a list, an entry is not a string, is empty or
holds a control character. A rule over a bound refuses with kind
`deny_rules_over_limit`: more than `MAX_DENY_RULES_PER_FILE` (256) in a file,
more than `MAX_DENY_RULES` (512) after removing duplicates and adding the
absolute form of one-slash rules, or a
rule longer than `MAX_DENY_RULE_CHARS` (512). Nothing is truncated; a rule the
user set is never dropped to make the run fit. The generated settings argument
is also held to `SETTINGS_ARGUMENT_LIMIT`.

**Anchoring.** Every rule is passed as written. Claude Code resolves `path` and
`./path` against the working directory in every settings source (a live probe of
inline `--settings` showed `Read(./secret.txt)`, `Read(secret.txt)` and
`Read(**/secret.txt)` each deny `secret.txt` and `sub/secret.txt` under the
working directory), so those need no help. The one form that loses its meaning
inline is a `Read` or `Edit` rule with exactly one leading slash, which Claude
Code defines as relative to the settings file's location: for it the original
rule is passed and an absolute rule `//BASE/path` is added beside it, where BASE
is, for the user's `settings.json`, the directory holding it (the configuration
directory, also when `CLAUDE_CONFIG_DIR` is set). For a project or local file
there is one added rule for the working directory and, when the file is in the
repository root or main checkout, another for the directory holding its
`.claude`, which covers either reading of the documentation. Adding a rule can only narrow a run. Rules for other tools
and rules starting `//` or `~/` are unchanged. Glob characters in BASE are
escaped. Duplicates are removed, keeping the first.

**How they are passed.** The rules are the `permissions.deny` array of the JSON
given to `--settings`, in all three modes (deny narrows `full` too):

```json
{"disableAllHooks":true,"permissions":{"deny":["Read(./secret.txt)","Edit(/out/x)","Edit(//base/out/x)"]}}
```

A rule is the user's own text, so a comma, bracket, quote or space in it is not
refused. It cannot become an allow rule or change the allow list because it is
only ever a string element of that array, which `serde_json` escapes: no text can
end the string or the array or add a key such as `permissions.allow`, and
`--allowedTools`, `--tools` and every other argument are built from delega's own
data and never from a rule. A string in a deny list is a deny pattern whatever
it holds, so a rule that is not well formed matches less and allows nothing.
This is held by a generated test and a test of hostile rules, which compare the
launch with and without rules and find only the added `permissions` key.

**What a user sees.** A note on every Claude run says that settings other than
deny rules were not applied (hooks, env, apiKeyHelper, plugins, skills, MCP
servers, instructions), and another gives the number of rules applied with the
path and count of each file, or says none were found. A settings file that was
ignored (see the roots above) and a main checkout that could not be identified
each get their own note naming the path and the reason. The mapping lists
`denyRules=N`. `dry_run` has a `deny_rules` object: the files read with their
rule counts, any local settings files ignored with the reason (`skipped`), `applied`, and `user_deny_rules`, the rules as they would be passed,
at most `DRY_RUN_DENY_RULES_SHOWN` (100) of them and each at most
`DRY_RUN_DENY_RULE_CHARS` (200) characters, with `omitted` counting the rest. The
rules are permission patterns the user wrote, not secrets, and they appear only
there; the `--settings` argument in a dry run shows their count in their place.
Text output prints the same under "user deny rules". The result's
`applied.deny_rules` (see [api.md](api.md#applied)) carries the same files,
`skipped` entries and `applied` count for a Claude run, and never a rule;
`applied.context` says that only the deny rules reached the worker
(`settings: "deny_rules"`) and that delega loaded no instructions. The notes stay.

**Codex.** Claude deny rules do not apply to Codex. The Codex adapter reads no
Claude Code settings and its dry run has an empty `deny_rules`.

### Strict

With `permissions.strict`, Claude Code refuses before start only when
`permissions.commands` is set and no command guard hook is configured, because
the commands cannot be granted exactly. The commands are not granted at all, so
the run would be narrower than asked, not wider; strict refuses it because the
request cannot be carried out as stated. The `delega` binary configures the hook
from its own executable, so this arises only when delega
cannot locate its own executable as an absolute path, or for an embedder that
sets no `ProviderOptions::hook`. With a hook, and in `full`, nothing is
refused. The run's other notes (settings not applied, instruction files) do not
describe a part of the permissions the provider fails to enforce, so strict
ignores them.

### What cannot be enforced

- `--setting-sources ""` means Claude Code loads no user, project or local
  settings file. Only their `permissions.deny` rules reach the worker (see
  [Deny rules](#deny-rules)). Not applied: hooks, `env`, `apiKeyHelper`,
  plugins, skills, MCP servers and settings-defined instructions. Every Claude
  run carries a note saying so.
- Claude Code loads no `CLAUDE.md` file and no auto memory into the worker's
  context automatically: `CLAUDE_CODE_DISABLE_CLAUDE_MDS=1` stops it loading
  any `CLAUDE.md` (the user's, the project's, a reference directory's) and the
  auto-memory index, and `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` turns auto memory
  off. The worker can
  still read such a file with its tools; the delegated preamble asks it to read
  applicable repository instructions. Every Claude run carries a note saying so.
- A working directory of `/` or the home directory makes the working-directory
  rules cover everything below it. The run is not refused; a note warns.
- `full` is the nine built-in tools. Sub-agents, MCP tools and background tasks
  stay off, and a note says so.
- `NotebookEdit` is not granted in `write`.
- The provider's own managed policy is applied by the provider over what delega
  passes, and no run reads it. `delega doctor` reports the local sources
  ([Managed settings](#managed-settings)).

### Resume, ephemeral, effort

`session.resume` is `--resume <id>`. `session.ephemeral` is
`--no-session-persistence`, and the result then has no `session_id`. `effort`
is `--effort`. `model` is `--model`. The request has no turn or budget fields, so
`--max-turns` and `--max-budget-usd` are not set; the time limit is the bound,
and both can be passed as `native_args`.

### Stream

Events are one JSON object per line. The result's `session_id` is the latest
`session_id` reported by an event up to the first terminal event. Ids on events
with a non-null `parent_tool_use_id` (a sub-agent's) are ignored, and ids on
events after the terminal event do not change it; they still count for the
session checks (the mismatch on a resumed run and the several-ids note on a
fresh one, see [Outcomes](#outcomes)). The
`result` event gives the answer (`result`), success (`subtype == "success"` and
`is_error` not true), errors, `usage` (`input_tokens` plus
`cache_creation_input_tokens`, reported as `input_tokens`; `output_tokens`;
`cache_read_input_tokens` as `cached_tokens`), `total_cost_usd`, `modelUsage`,
`model_usage_since_last_result` and `permission_denials`.

The result's `usage` token counts are the run's own and become the result's
`usage` tokens. `total_cost_usd` and `modelUsage` are the session's totals and
become `session_usage`: `cost_usd` is `total_cost_usd`, and the tokens are the
sums over the models in `modelUsage` (`inputTokens` plus
`cacheCreationInputTokens` as `input_tokens`, `outputTokens`,
`cacheReadInputTokens` as `cached_tokens`). The run's own `cost_usd` is the sum
of `costUSD` over `model_usage_since_last_result`, used only when that object
has at least one entry and every entry has a usable (finite, non-negative)
`costUSD`; there are no partial sums. Otherwise a run that did not resume takes
`total_cost_usd`, and a resumed run has no `usage.cost_usd`. Negative and
non-finite values are ignored throughout. Messages with `parent_tool_use_id` are sub-agent messages
and are not used as the worker's text. When the result carries no text, the last
assistant message is used and a note says so.

A failed result's message is its `errors` joined with `; `; when it has no
`errors`, its `result` text (an API failure carries its reason there); with
neither, a sentence naming the subtype. A failed result that is not a turn or
budget limit gets a [provider failure kind](#provider-failure-kinds) from three
sources: the result itself; the last root assistant message, when that message
has `is_api_error_message: true`; and the latest root `rate_limit_event`. A
later root assistant message without `is_api_error_message` clears both, since
the run went on after them. Sub-agent events are not used, and a sub-agent's
`result` (one with a non-null `parent_tool_use_id`) does not end the run. Fields read: the assistant message's `error`, and `api_error`,
`api_error_type` and `api_error_status` on either. Every structured field is
checked before the result text, so the text rules come last whatever their
kind. In order:

- `provider_usage_limit`: `api_error` `usage_limit_reached`, or the latest
  `rate_limit_event` has `rate_limit_info.status` `rejected` and overage does
  not cover it (`isUsingOverage` is not true and `overageStatus` is not
  `allowed` or `allowed_warning`).
- `provider_billing`: `error` or `api_error_type` `billing_error`.
- `provider_auth`: `error` `authentication_failed`, `oauth_org_not_allowed`,
  `account_on_hold`, `verification_required` or `cloud_credential_error`;
  `api_error_type` `authentication_error` or `permission_error`; or status 401
  or 403.
- `provider_model_unavailable`: `error` `model_not_found`, or
  `api_error_type` `not_found_error` with status 404.
- `provider_capacity`: `error` `rate_limit` or `overloaded`, `api_error_type`
  `rate_limit_error` or `overloaded_error`, or status 429, 503 or 529.
- `provider_context_exhausted`: the result text starts `Prompt is too long`.
- `provider_capacity`: the result text starts `API Error: 429`, `503` or `529`.

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

With `permissions.strict` (`--strict`), a Codex `read` or `write` run is refused
before start with `permissions_not_enforceable`, because the request cannot be
enforced as stated. The message names these parts: reads are not confined to the
working directory and `permissions.directories`; commands that need no write
access and no network access can run although the request allows only the
commands it lists (in `write`, commands that write inside the working directory
also run); and, when `permissions.commands` is set, that it has no
effect. A Codex `full` run is not refused: it asks for nothing confined.

### What cannot be enforced

- Reads are not limited to the working directory or the listed directories.
- Commands are not limited to the list.
- Denials are reported only for commands the stream marks `declined`. Other
  blocked actions appear as failed commands and are not in `permissions.denied`.
  A note says so on every run.
- `--ignore-user-config` ignores the user's Codex configuration, but Codex may
  still load its own `AGENTS.md` files.
- The provider's own managed policy is applied by the provider over what delega
  passes, and no run reads it. `delega doctor` reports the local sources
  ([Managed settings](#managed-settings)).

### Stream

`thread.started.thread_id` is the session id; a `thread.started` after the
terminal event does not change it but still counts for the session checks (the
mismatch on a resumed run and the several-ids note on a fresh one). A message item that is not marked
`commentary` is the answer. `turn.completed` is success and carries `usage`
(`input_tokens`, `output_tokens`, `cached_input_tokens`,
`reasoning_output_tokens`); `turn.failed` is failure with its message, and its
`usage`, when it has one, is read the same way. These counts are the thread's
totals, also on a resumed run; Codex reports no per-turn figure. They become
`session_usage`, and `usage` only for a run that did not resume. For a resumed
run delega does not know the run's own consumption: `usage` is empty and only
`session_usage` is reported, when Codex reported totals (a run that times out
or fails before any terminal event has neither). The difference between two runs' `session_usage`
is one run's own only if nothing else used the thread in between (another
worker or a CLI `--resume` on the same thread, or a failed or timed-out run
that reported no totals); delega does not compute it. `error`
events are kept: they fail the run only if no terminal event follows.

The [provider failure kind](#provider-failure-kinds) comes from the
`turn.failed` event, or from the last `error` event when no terminal event
follows. An `error` event or `turn.failed` with a `codex_error_info` and no
message counts, with the message `Codex reported <name>`. A structured `codex_error_info` (on the event or its `error`; a string,
or an object with one key) decides first: `usage_limit_exceeded` is
`provider_usage_limit` and `context_window_exceeded` is
`provider_context_exhausted`. Next, a message that is a JSON error document is
read by `error.code` (`insufficient_quota`, `context_length_exceeded`,
`invalid_api_key`, `model_not_found`, `rate_limit_exceeded`, ...) or
`error.type`. Last, the message's words, case-insensitive, in the order of the
[precedence](#provider-failure-kinds) (Codex text names no billing failure):

- `provider_usage_limit`: "usage limit", "exceeded your current quota" or
  "insufficient_quota".
- `provider_auth`: "unexpected status 401" or "unexpected status 403".
- `provider_model_unavailable`: "model is not supported", "model_not_found",
  or ``model `name` does not exist`` with the name in backticks, single or
  double quotes directly after "model", where "model" starts a word.
- `provider_context_exhausted`: "context window".
- `provider_capacity`: "unexpected status 429", "503" or "529", "rate limit",
  "overloaded" or "at capacity".

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
   from `--env NAME` (repeatable, short `-e`) and from `[env] pass` in the
   user-level [configuration file](api.md#configuration-file), the file's names
   first; the rules and refusals below are the same for both. A name that is not set in the parent
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
   `--env NAME=VALUE` and from `[env.set]` in the configuration file (the file's
   entries first, so a flag for the same name wins), sets variables after the rules above, whatever the parent
   environment holds. The names are checked as in group 4: an invalid,
   calling-session or `DELEGA_*` name is `refused`, and a secret-looking name
   gets a note. Prefer `NAME` (group 4) for secrets on the command line, since a
   value there is visible in the process table and shell history. The adapter's variables (`CLAUDE_CODE_DISABLE_BACKGROUND_TASKS`,
   `CLAUDE_CODE_DISABLE_AUTO_MEMORY`, `CLAUDE_CODE_DISABLE_CLAUDE_MDS`,
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
  variables of the caller were withheld; M were passed`. The same counts, by
  reason, and the names the caller gave (`--env`, `[env]`) are in the result's
  `applied.env` (see [api.md](api.md#applied)); the note stays. `M` counts every
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
- Deny rules passed as `permissions.deny` in the `--settings` JSON with empty
  settings sources are applied, on 2026-11-18 with Claude Code 2.1.328: a project
  deny rule blocks `Read`, hides the file from `Grep`, and overrides an allowed
  command. Relative rules in inline settings (`./x`, `x`, `**/x`) resolve against
  the working directory and match at any depth. Not run: the added absolute form
  of one-slash rules.
- With Claude Code 2.1.329 and Codex 0.170.0 (#114), a follow-up that resumes a
  real session reports the requested id on both.
- With Claude Code 2.1.329, `--resume` with an unknown id exits 1 after a
  `result` event with subtype `error_during_execution`, `is_error: true`,
  `errors: ["No conversation found with session ID: <id>"]` and the requested
  id as `session_id`. It becomes kind `session_not_found`.
- With Codex 0.170.0, `resume` with an unknown id exits 1 with no events on
  standard output and the standard error `Error: thread/resume: thread/resume
  failed: no rollout found for thread id <id> (code -32600)`. It becomes kind
  `session_not_found`.
- With Claude Code 2.1.329 (#116), an unknown `--model` writes a root
  `assistant` event with `error: "model_not_found"`, `api_error_status: 404`,
  `api_error_type: "not_found_error"`, `is_api_error_message: true` and the
  text "There's an issue with the selected model (no-such-model). It may not
  exist or you may not have access to it. Run --model to pick a different
  model.", then a `result` with subtype `success`, `is_error: true`, the same
  status and type, `terminal_reason: "api_error"`, that text in `result` and no
  `errors`, and exits 1. It becomes kind `provider_model_unavailable`.
- With Claude Code 2.1.329 (#116), an invalid API key gives the same shape with
  `error: "authentication_failed"`, `api_error_status: 401`, `api_error_type:
  "authentication_error"`, `api_error_origin: "first_party"` and the text
  "Invalid API key · Fix external API key". It becomes kind `provider_auth`.
  The 2.1.329 executable lists these values for the assistant `error`:
  `authentication_failed`, `oauth_org_not_allowed`, `account_on_hold`,
  `verification_required`, `billing_error`, `rate_limit`, `overloaded`,
  `invalid_request`, `model_not_found`, `server_error`, `unknown`,
  `max_output_tokens`, `cloud_credential_error`. Only the two above were run.
- With Claude Code 2.1.329 and Codex 0.170.0 (#115), a start writing about
  600 output tokens and two one-word follow-ups resuming it reported:

  | Run | Claude `output_tokens` | Claude `total_cost_usd` | Codex `input_tokens` | Codex `output_tokens` | Codex `cached_input_tokens` |
  | --- | --- | --- | --- | --- | --- |
  | start | 601 | 0.0283 | 16,229 | 603 | 13,440 |
  | follow-up 1 | 4 | 0.0374 | 33,074 | 608 | 26,880 |
  | follow-up 2 | 3 | 0.0388 | 49,937 | 613 | 43,648 |

  Claude Code's `usage` token counts are per run and `total_cost_usd` is the
  session's. Its result also has `modelUsage` (session totals per model) and
  `model_usage_since_last_result` (this run's own, per model): a start had
  `total_cost_usd` 0.01142 and `model_usage_since_last_result.<m>.costUSD`
  0.01142; the resumed run had `total_cost_usd` 0.0173514,
  `modelUsage.<m>.costUSD` 0.0173514, `model_usage_since_last_result.<m>.costUSD`
  0.0059314, `model_usage_since_last_result.<m>.outputTokens` 4 and
  `modelUsage.<m>.outputTokens` 205. Every Codex `turn.completed.usage` count
  on a resumed run is the thread's total.
- With Codex 0.170.0 (#116), `codex exec --json` carries no structured codes
  for these failures, only text. An unknown model with a ChatGPT login gives an
  `error` event and a `turn.failed` with the message "The 'no-such-model' model
  is not supported when using Codex with a ChatGPT account." and exits 1 (kind
  `provider_model_unavailable`). An invalid API key gives ten `error` events
  "Reconnecting... n/5 (unexpected status 401 Unauthorized: Incorrect API key
  provided: ...)" and a `turn.failed` with "unexpected status 401
  Unauthorized: Incorrect API key provided: ...", and exits 1 (kind
  `provider_auth`).

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
| `CLAUDE_CODE_DISABLE_CLAUDE_MDS=1` and `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` stop Claude Code loading `CLAUDE.md` files and the auto-memory index into the worker's context automatically, as Claude Code documents | With Claude Code 2.1.329, a worker started this way listed no `CLAUDE.md`, `AGENTS.md` or auto-memory file in its context (#86, #106). A note on every run says what is not loaded automatically. |
| A Claude failure with `permission_denials` and no `error` or `api_retry` event is a stop caused by the denial | Anything else with a denial is `failed`. |
| Claude Code's failed `errors` name the denied tool or speak of permission when a denial stopped it | Matching is by those words. Anything else with a denial is `failed`, never `refused`. |
| Claude Code 2.1.314 at a usage limit, as an earlier runner recorded it, writes a root assistant message with `error: "rate_limit"`, `is_api_error_message: true`, `api_error_status: 429`, `api_error: "usage_limit_reached"` and `api_error_params.rate_limit_info` (`status: "rejected"`, `overageStatus`, `isUsingOverage`), with text such as "You've hit your session limit · resets 1pm (America/Los_Angeles)", then a failed `result` with `api_error: "usage_limit_reached"` and that text, and exits 1; a root `rate_limit_event` with `rate_limit_info.status: "rejected"` can also appear | `usage_limit_reached` or a rejected `rate_limit_event` that overage does not cover is `provider_usage_limit`, ahead of the `rate_limit` and 429 it also carries. With `isUsingOverage` true or `overageStatus` `allowed` or `allowed_warning`, overage covers it. |
| Claude Code capacity failures carry `api_error_status` 429, 503 or 529 with text like "API Error: 529 overloaded", or assistant `error` `rate_limit` or `overloaded`; `billing_error` and the account `error` values other than `authentication_failed` reach the stream like the live shapes; a context overflow is a failed result whose text starts "Prompt is too long" | Matched as described in the Claude Code [Stream](#stream) section. Not run live. |
| Codex at a usage limit, as an earlier runner recorded it, writes `error` and `turn.failed` messages "You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at ...", and `codex_error_info: "usage_limit_exceeded"` can appear on such events; capacity failures say "unexpected status 429", "503" or "529", "rate limit", "overloaded" or "at capacity"; a context overflow says "context window" or carries `codex_error_info` `context_window_exceeded` | Matched as described in the Codex [Stream](#stream-1) section. A context overflow was `provider_usage_limit` before #116 and is now `provider_context_exhausted`. |
| Claude Code names `error_max_turns` and `error_max_budget_usd` as result subtypes | They become the `provider_turn_limit` and `provider_budget_limit` error kinds. |

## Managed settings

Both providers read managed (administrator) settings themselves, above anything
on the command line. delega does not apply them and cannot turn them off, so
they can change what a worker gets with nothing in the result saying why.
`delega doctor` ([api.md](api.md#doctor)) looks for the local sources of each
provider and lists the keys present in them that bear on what delega passes at
launch, each with a category. It says once per provider that these keys can
change how the provider treats what delega passes, and points at the provider's
reference. It does not say what a key does, or whether a key is on: read the
file and the provider's reference for that.

It reports the fixed paths below, counts, key names and categories, never a
value. Nothing it prints is copied from a file or a directory. A drop-in file or
a user's preferences directory can be named anything, so each of those is one
source with a count. A key is named only when it is one doctor knows, under
doctor's own spelling; a key of a Codex file that doctor does not know is
counted (`other_keys`) and not named. It is a notice: it adds no problem and
does not change `ready` or the exit status.

**Sources looked for.** Windows is out of scope.

| Provider | Source | Where | Reported |
| --- | --- | --- | --- |
| Claude Code | `managed-settings.json` | `/Library/Application Support/ClaudeCode` on macOS, `/etc/claude-code` on Linux | The keys below that are present. |
| Claude Code | `managed-settings.d/*.json`, hidden files left out | The same directory | The directory once, with the number of files, how many could not be read, and the keys below from all of them together. No file is named. |
| Claude Code | `managed-mcp.json` | The same directory | Its presence, with a pointer to Claude Code's [managed MCP reference](https://code.claude.com/docs/en/managed-mcp), which describes what the file does to `--strict-mcp-config` and `--mcp-config`. delega passes both on every run. Server names are not read. |
| Claude Code | Managed preferences domain `com.anthropic.claudecode` | macOS: `/Library/Managed Preferences/com.anthropic.claudecode.plist` and the same file in each user's directory there | The system file as present, not inspected; the users' files as one source with a count. No user is named. |
| Codex | `requirements.toml` | `/etc/codex` on macOS and Linux | The top-level keys Codex reads, by name, and a count of the others. |
| Codex | `managed_config.toml` (legacy) | `/etc/codex` on macOS and Linux | The top-level keys listed below, by name, and a count of the others. |
| Codex | Managed preferences domain `com.openai.codex` (`config_toml_base64`, `requirements_toml_base64`) | macOS: `/Library/Managed Preferences/com.openai.codex.plist` and the same file in each user's directory there | As for Claude Code. |

A file that parses is `read`. An empty `managed-settings.json` or drop-in file is
read as `{}`. A file that is there and cannot be read or parsed (no permission,
not a regular file, over 4 MiB, not JSON or TOML, no object at the top) is
`unreadable`, with no content and no error text. Values are skipped, not
converted, so a number too large for doctor to hold does not make a file
unreadable or hide its keys. Managed preferences are `not_inspected`: doctor
does not open them, so it cannot say which keys they set, and a file in another
user's directory does not apply to this user.

A file is opened without blocking and must be a regular file once open, so a
pipe or a device in its place is `unreadable` and cannot hang doctor. Symbolic
links are followed, as the providers follow them. The drop-in directory is read
up to 64 files, 4096 directory entries and 4 MiB in all, counted as bytes read
from disk whether or not they are text, and never a byte more: a file that
grows while it is read is cut off at what is left of the total. When some of its files cannot be read
the directory is `partly_read` and the report says how many. When a limit stops
the scan the directory is `partly_read` with `limit_reached`, even if no drop-in
file was seen before the limit, because there may be files that were never
looked at. The same holds for the users' preferences: a listing cut off at 4096
entries is reported as `partly_read` with `limit_reached`. The size of a file is
taken before and after it is read. A file whose size changed meanwhile, or that
holds more than was read, is not reported as read. A single file is then
`partly_read` with no keys, and that is the only case in which a single file has
`files` (one found, one not read, `limit_reached`). A drop-in file is counted as
not read and marks its directory `partly_read` with `limit_reached`.

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
| `sandbox` | `sandbox.enabled`, `sandbox.filesystem.allowManagedReadPathsOnly`, `sandbox.network.allowManagedDomainsOnly` |
| `authentication` | `forceLoginMethod`, `forceLoginOrgUUID`, `enforceAnthropicSignIn`, `allowedProviders`, `apiKeyHelper` |
| `environment` | `env` (the variable names in it are not listed) |
| `model` | `availableModels`, `deniedModels`, `modelOverrides` |
| `version` | `requiredMinimumVersion`, `requiredMaximumVersion` |
| `policy` | `policyHelper` |

They are listed because each bears on something delega passes at launch: the
tools granted with `--allowedTools` and the deny rules and command hook in
`--settings`, `--permission-mode dontAsk`, `--strict-mcp-config` and
`--mcp-config`, the `--add-dir` directories, `--model`, and the environment a
worker gets, which carries its credential.

Two keys are not listed: `permissions.disableBypassPermissionsMode` and
`permissions.defaultMode`.

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

## Not done

- **User instructions for Claude Code.** The earlier project also copied the
  user's `CLAUDE.md` into the launch. This layer does not read it. The
  `permissions.deny` rules are carried over (see [Deny rules](#deny-rules)).
- **Managed settings.** No run reads or applies them, and a result does not say
  that one changed it. `delega doctor` names the local sources and the keys in
  them that matter ([Managed settings](#managed-settings)); settings a provider
  fetches from its server cannot be seen.
- **Command shape in the result.** The contract's `provenance` has no field for
  it. `dry_run` reports the shape; a run records the executable and version.
- **Live asking** (`on_request: ask`), as in [api.md](api.md).
- **Stopping at the final result.** A provider that writes its result and then
  does not exit holds the run until the time limit.
- **Usage during a run.** Usage is taken from the terminal event only.
- **A resumed Codex run's own usage.** Codex reports only the thread's totals,
  so a resumed run's `usage` is empty and only `session_usage` is reported,
  when the run reported totals at all.

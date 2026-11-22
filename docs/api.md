# The delega API

This document describes the surface: the contract, the command line, the MCP
server and the skills. The request and result contract, the CLI commands `run`,
`mcp`, `plugin install`, `plugin uninstall`, `doctor`, `schema`, `skill` and
`version`, the provider layer behind `run`, the library `Supervisor`
([supervisor.md](supervisor.md)), the MCP server over it and the user-level
[configuration file](#configuration-file) are built. Josh
settled the open questions on 2026-11-18; the decisions and what was deferred are
listed at the end. Parts marked **Later** are not built.
[Where each part comes from](#where-each-part-comes-from) maps each part to its
source in [due](https://github.com/joshrotenberg/due) and
[counterpart](https://github.com/joshrotenberg/counterpart).

## The model

Three nouns, used the same way on every surface.

- **Request.** What to run: a provider, an assignment, a working directory, what
  the worker may do, and how long it may take.
- **Worker.** One provider conversation. It holds the provider's session, so a
  later assignment continues with the earlier context. A worker takes one
  assignment at a time.
- **Run.** One assignment given to a worker. A run ends in exactly one
  **outcome** and produces one **result**.

One request starts a worker and its first run. A follow-up starts another run in
the same worker.

## Outcomes

Every run ends in exactly one of these. There is no sixth state, and a run does
not stay open without a deadline.

| Outcome | Meaning |
| --- | --- |
| `completed` | The provider finished the assignment and returned a final answer. |
| `failed` | The provider or delega hit an error. `error.kind` says which. |
| `timed_out` | The run's time limit passed. |
| `interrupted` | The caller stopped it, or the caller went away. |
| `refused` | The run did not go ahead: the request was invalid, a limit was reached, or a permission the run needed was denied and it could not continue. |

`completed` says the provider finished. It does not say the work is correct;
checking that belongs to the caller.

A run that was denied a permission but still returned a final answer is
`completed`, with the denial listed in `permissions.denied`. It is `refused`,
with kind `permission_denied`, only when the run stopped because of the denial
([providers.md](providers.md#outcomes) gives the rule).

Error kinds are listed in [providers.md](providers.md#error-kinds). The
supervisor adds the refusals `name_in_use`, `limit_reached`, `queue_full`,
`worker_closed`, `no_session` and `preflight_timeout`, and `delega mcp` adds
`cwd_required` (see [MCP](#mcp)). `delega run` adds `runtime_unavailable` and
`signals_unavailable` (see [Command line](#command-line)). `delega run` and
`delega mcp` add `config_unreadable` and `config_invalid`, for a configuration
file they cannot use (see [Configuration file](#configuration-file)).

## The request

One JSON object, versioned. The CLI builds it from flags, MCP from tool
arguments, and the library from a struct. A partial request saved as a file is a
**profile**; explicit values override it.

| Field | Type | Notes |
| --- | --- | --- |
| `schema_version` | integer | Must be 1. Required in a profile file; MCP arguments and the library default it to 1. |
| `provider` | `claude` or `codex` | Required after merging profile and flags. |
| `prompt` | string | The assignment, at most 4 MiB, not blank. Always sent to the provider on stdin. |
| `cwd` | path | Working directory of the worker. For `delega run` and the library, absent means the caller's working directory and a relative path resolves against it. MCP `start` requires an absolute `cwd` (`cwd_required`, see [MCP](#mcp)). Must exist and be a directory, or the run is refused `cwd_unusable`. |
| `name` | string | Optional label for the worker, at most 64 characters. Under the supervisor (MCP, library) it must be unique among live workers, or the start is refused `name_in_use`; a closed worker's name is free again. |
| `instructions` | string | Extra system instructions for the worker, at most 256 KiB. `delega run` takes it only from a profile. |
| `model` | string | Provider model name or alias, at most 256 bytes, not starting with `-`. Absent keeps the provider default. |
| `effort` | `low`, `medium`, `high` | Absent keeps the provider default. |
| `timeout_secs` | integer | Limit for the run in whole seconds, 1 to 86400. Default 600 (10 minutes). The limit applied is stated in the result as `timing.timeout_secs`. |
| `permissions` | object | See below. |
| `answer_schema` | JSON Schema | Ask for a structured answer and validate it. An object or a boolean, at most 256 KiB as compact JSON; one that does not compile is refused `invalid_answer_schema`. |
| `session` | object | `ephemeral: bool`, or `resume: <provider session id>` (at most 256 bytes) to continue saved history in a new worker. Not both. A resumed run that reports another session fails with `session_mismatch` only when it would otherwise have completed; any other outcome stands and a note names both ids. One whose session the provider cannot find fails with `session_not_found` ([providers.md](providers.md#outcomes)). |
| `native_args` | list of strings | Provider options passed through, restricted by an allowlist; a rejected option is refused `native_arg_rejected`. At most 64 entries. The escape hatch. |

Every length and count bound is a constant in
`crates/delega-core/src/contract/limits.rs`. The request schema
(`delega schema request`) carries the ranges and list counts, not string
lengths. A value outside a bound is refused `invalid_request`, with every
problem listed.

### Permissions

What a worker may do is part of the request, in provider-neutral terms, and the
result reports what happened.

| Field | Values | Meaning |
| --- | --- | --- |
| `permissions.mode` | `read` (default), `write`, `full` | `read`: may read the working directory, no writes, no commands except those listed in `permissions.commands`. `write`: may also edit files in the working directory; commands are still only those listed. `full`: no restriction from delega. |
| `permissions.commands` | list of exact command strings | Shell commands the worker may run, matched exactly, at most 256 of 4096 bytes each. They add to `read` and `write`; `full` already allows every command. |
| `permissions.directories` | list of paths | Extra directories the worker may read, at most 64. Each must exist, or the run is refused `directory_unusable`. A relative entry is relative to the caller's working directory for `delega run`, and to `cwd` for MCP `start`. |
| `permissions.on_request` | `deny` | What happens when the provider asks for something not granted: answered no at once and recorded in the result. **Later:** `ask`, which surfaces the request to the caller as a pending interaction, with `permissions.ask_timeout` for how long it waits before it is answered no. |
| `permissions.strict` | `false` (default), `true` | `true`: when the provider cannot enforce part of the request as stated, the run is `refused` before start with `error.kind` `permissions_not_enforceable`, and the message names each part (see [providers](providers.md) for what each provider cannot enforce). `false`: the run goes ahead and a `provider_notes` entry says what is wider than requested. `full` asks for nothing confined, so it is never refused for this. |

Each mode maps to concrete provider settings (Claude tool lists, hooks and
permission mode; Codex sandbox and approval policy). The mapping is documented
per provider, and where a provider cannot enforce part of it the result says so
in `provider_notes`, or, with `permissions.strict`, the run is refused before it
starts. The refusal kind is `permissions_not_enforceable`; the message reads
`permissions.strict is set and <provider> cannot enforce these permissions as
stated: <part>; <part>`. A dry run reports the same refusal. With Codex,
`strict` refuses every `read` and `write` request, because Codex confines
neither reads nor commands. With Claude Code it refuses only
`permissions.commands` when no command guard is configured.

`--strict` only turns the field on: a profile's `strict: true` stays on for
`delega run`, because there is no flag to turn it off. MCP `start` can pass
`permissions.strict: false` to override a profile.

Refusals before start come in a fixed order, and the first one found is
reported. For `delega run` the first of all is the configuration file
(`config_unreadable`, `config_invalid`, or `invalid_env_name` or
`forbidden_env_name` for a name in its `[env]`), because the request is built
from it; see [Configuration file](#configuration-file). Then `invalid_request`, `native_arg_rejected`, `invalid_answer_schema`,
`cwd_unusable` or `directory_unusable`, `provider_not_found`, Claude Code's
`settings_unreadable` and `deny_rules_over_limit` from reading the settings
files, then the provider's own mapping of the request (for Claude Code:
`cwd_unusable` or `directory_unusable` for a path it cannot write as a rule,
`unsupported_command`, and, for a generated settings argument over its bound,
`deny_rules_over_limit` when user deny rules are part of it and
`unsupported_command` otherwise), then `permissions_not_enforceable`, then
`invalid_env_name` and `forbidden_env_name`.
So a strict request that Codex cannot enforce and that also has an invalid
environment name is refused as `permissions_not_enforceable`; without strict it
is refused as `invalid_env_name`. `unsupported_provider_version` is found later,
after the provider's version is read; if that probe's processes could not be
confirmed stopped, the result is `failed` with the same kind. For Codex, a failed
listing of its MCP servers is then `failed` with kind
`provider_preparation_failed`. A dry run starts no probe, so it reports neither.

Under the supervisor, `start` first checks the request (`invalid_request`), the
name (`name_in_use`) and the live-worker limit (`limit_reached`), then this
list, bounded by 10 s (`preflight_timeout`). `delega mcp` adds one refusal,
`cwd_required`, for a `start` whose merged request (after any `profile`) has no
`cwd` or a relative one. It is found after `invalid_request`, a name in use and
the live-worker limit, and before every provider check above, so no provider
check or process runs. See [MCP](#mcp). `delega run` has no such refusal.

`permissions.strict` is an optional field with a default, so a request without it
means what it meant before and `schema_version` stays 1. The result does not
echo it.

The first version only denies. Asking needs a long-lived provider session,
which is due's resident engine and the subject of issue 1; `deny` works with
one process per run.

## The result

One JSON object, versioned, the same from every surface.

| Field | Notes |
| --- | --- |
| `schema_version` | |
| `run` | Run id. |
| `worker` | Worker id and `name`. Present only under the supervisor (MCP, library), and absent for a refused `start`; `delega run` results have none. |
| `provider` | Absent only on a refused result for a request that named no provider. |
| `outcome` | One of the five above. |
| `text` | The final answer, or empty. |
| `structured` | The structured answer when `answer_schema` was given. |
| `structured_valid` | Whether `structured` satisfied the schema; present with `structured`. |
| `error` | `kind`, `message`, and `provider_exit_code` when the provider process exited with one. Present unless `completed`. |
| `cleanup` | `confirmed: true` when a final sweep after the provider exited found nothing left in its process group, its session or its tracked descendants; otherwise `confirmed: false` with `pids`, `process_groups` and `reason`. Two gaps can still report `confirmed: true` while a descendant runs; they are described in `crates/delega-core/src/process/mod.rs`. |
| `permissions` | `mode` applied, and `denied`: requests answered no. **Later:** `asked`, requests the caller answered. |
| `provider_notes` | Where this provider did not do what the request asked, or did it differently. |
| `applied` | What delega applied: the configuration file, which layer set each request field, the environment, the deny rules and the context mode, as names, counts, paths and digests. Optional; see [`applied`](#applied). |
| `session_id` | The provider's own session id, for the provider's `resume`. |
| `usage` | This run's own consumption: tokens in and out, cached, reasoning, and `cost_usd`. A field is present only when delega knows the run's own value. See [Usage](#usage). |
| `session_usage` | The provider's totals for the whole session through this run, in the same fields, when the provider reports totals. For a run that started its session, the session is the run. |
| `timing` | Queued, started and finished times, duration, and the time limit applied as `timeout_secs`. |
| `provenance` | delega version, and the provider executable path and version. `build` is in the schema but not filled yet. |

A caller that only needs to know what happened reads `outcome`, `text`, `error`
and `cleanup`.

### Usage

On a resumed run the providers report session totals for some values, so
`usage` is not copied from the provider as is:

- Claude Code: the tokens are the run's own. The run's own `cost_usd` is the
  sum of `costUSD` over `model_usage_since_last_result`, used only when that
  object has at least one entry and every entry has a usable (finite,
  non-negative) `costUSD`; there are no partial sums. Otherwise a run that did
  not resume takes `total_cost_usd` (the session is the run), and a resumed run
  has no `cost_usd`. `session_usage` has the session's total cost and tokens.
- Codex: only the thread's totals are reported. They are `session_usage`, and
  `usage` too for a run that did not resume. For a resumed Codex run delega
  does not know the run's own consumption: it has no `usage` and reports only
  `session_usage`, through `delega run`, MCP and the library alike.

Both fields come from the provider's terminal event. A run that times out, is
interrupted or fails before any terminal event has neither; one that hangs
after its terminal event keeps what that event reported.

The difference between two runs' `session_usage` is one run's own consumption
only if nothing else used the provider session in between: another worker or a
`delega run --resume` on the same session, or a failed or timed-out run that
reported no totals, all add to it. delega does not compute it.

Compact reads leave out `usage` and `session_usage`, as they do provenance.

### `applied`

`applied` is the structured record of what delega applied to the run, for a
caller that wants to check it without reading prose. `provider_notes` still say
the same things in words and none of them was removed. The object is optional, so
`schema_version` stays 1 and an older reader ignores it. Every value is a name, a
count, a path or a digest: never the value of an environment variable and never
the text of a deny rule. Keys are in sorted order.

```json
{
  "config": {"path": "/home/u/.config/delega/config.toml", "sha256": "9f2c...e1"},
  "sources": {
    "cwd": "flag",
    "model": "config",
    "permissions.mode": "profile",
    "permissions.on_request": "default",
    "permissions.strict": "default",
    "prompt": "flag",
    "provider": "config",
    "session.ephemeral": "default",
    "timeout_secs": "default"
  },
  "env": {
    "passed": {"total": 16, "base": 11, "network": 0, "provider": 0,
               "named_by_caller": 1, "set_by_caller": 1, "set_by_delega": 3},
    "withheld": {"total": 31, "never_passed": 2, "not_allowlisted": 29},
    "named": ["AWS_REGION", "TOOLCHAIN_HOME"]
  },
  "deny_rules": {
    "files": [{"path": "/home/u/.claude/settings.json", "rules": 3}],
    "skipped": [],
    "applied": 3
  },
  "context": {"settings": "deny_rules", "instructions": false}
}
```

| Part | Content | When it is absent |
| --- | --- | --- |
| `config` | `path` of the configuration file that was read and the `sha256` of the bytes read (64 lowercase hex digits). | No file was read: none exists at the place looked for, or none is looked for. A file that is refused is not reported, and a command that refuses for it has no `applied`. `delega mcp` reads the file once at start, so every run of a server reports the file as it was then. |
| `sources` | For each request field that ended up set, the layer that set it: `default`, `config` (the file's `[defaults]`), `profile`, `flag` (`delega run`) or `argument` (an MCP tool argument). | The caller did not say, as when a library caller starts a run without `ProviderOptions::origin`. It is present, with the fields known so far, on a refusal that happens after the request was merged. |
| `env` | `passed` counts the variables the worker received, in total and by reason; `withheld` counts the caller's variables that were not passed, in total, those that can never be passed (calling-session variables and every `DELEGA_*`) and the rest; `named` lists the names given with `--env` or in the file's `[env]`, whether or not the variable was set. | The run was refused before it was prepared (see below). |
| `deny_rules` | The settings files read with their rule counts (`files`), the ones ignored with the reason (`skipped`), and the number of rules passed (`applied`). | Codex runs, which read none, and a refusal before the settings files were read. |
| `context` | `settings`: how Claude Code's settings reached the worker, `deny_rules` being the only mode today (the deny rules and nothing else); `instructions`: the launch policy for instruction files, `false` today: delega sets `CLAUDE_CODE_DISABLE_CLAUDE_MDS=1` and `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`, so Claude Code does not load `CLAUDE.md` files (the user's, the project's or a reference directory's) or auto memory into the worker's context automatically, as a `provider_notes` entry says. It stays `false` when the worker reads such a file with its tools. | Codex runs, which have no such mode to report, and a refusal before it was decided. |

`env`, `deny_rules` and `context` are filled in together, when the run has been
prepared: the request validated, the working directory and executable found, the
settings files read, the permissions mapped and the environment resolved. A
refusal before that point (an invalid request, an unusable working directory, a
rejected native argument, a missing provider executable, unusable settings files,
a permission that `strict` cannot enforce, an invalid or forbidden `--env` name)
therefore has `config` and `sources` only. So does a refusal that comes from the
supervisor, such as a name in use or the worker limit, and a run stopped in the
queue before it started. A run that was prepared and then ended without finishing
(a failure, a time limit, an interrupt, or a refusal at start such as an
unsupported provider version) has every part that applies. A refusal for a
configuration file that cannot be used has no `applied`, because no usable file was read
and no request was built. Two internal-error endings carry `config` and `sources` only,
even for a run that was prepared: a run whose task in the supervisor panicked, and a
result the supervisor replaced because it failed validation. `applied` is absent altogether when nothing is known.

`sources` keys are the request fields (`provider`, `prompt`, `cwd`, `name`,
`instructions`, `model`, `effort`, `timeout_secs`, `answer_schema`,
`native_args`) and, as dotted names, `permissions.mode`, `permissions.commands`,
`permissions.directories`, `permissions.on_request`, `permissions.strict`,
`session.ephemeral` and `session.resume`; `schema_version` is not a key. A key is
present when the field has a value. Five fields have built-in defaults and are
always present: `timeout_secs`, `permissions.mode`, `permissions.on_request`,
`permissions.strict` and `session.ephemeral`, as `default` unless a layer set
them. The other fields appear only when a layer set them, so an absent `model`
means none was set. The layers are recorded where they are merged (the profile
merge in `delega_core::profile`, called by `delega run` and by MCP `start`), not
worked out afterwards from the values. A field a merge clears loses the layer that set
it: an explicit `ephemeral` removes a `resume` that a lower layer set, and
`session.resume` is then absent; an explicit `resume` removes an `ephemeral` that a
lower layer set, and `session.ephemeral` is then `default`, its built-in value. For a follow-up, `sources` are those of the worker's `start`, with `prompt`
(and `timeout_secs` when the call gave one) as `argument`.

A dry run carries the same object, as `dry_run.applied`. MCP `read` returns it in
both detail levels: the object is small, so a compact `read` includes it, next to
`provider_notes`.

## Command line

The CLI runs one assignment in the foreground and exits. It is what a script or
another agent calls.

```sh
delega run --provider codex -p "Summarize this repository"
delega run --provider claude --stdin --output json < task.txt > result.json
delega run --request profile.json --stdin --timeout 20m --write < task.txt
delega run --provider codex --resume SESSION_ID -p "Now add tests"
```

| Command | Purpose |
| --- | --- |
| `delega run` | Run one assignment and print the answer, or the result as JSON. |
| `delega mcp` | Serve the MCP tools over stdio, with `-e/--env NAME[=VALUE]` (repeatable) as for `run`. See [MCP](#mcp). |
| `delega plugin install` | Install the plugin and skills into Claude Code, Codex or both, through the hosts' own commands. `--dry-run` shows what it would do. See [Install and uninstall](#install-and-uninstall). |
| `delega plugin uninstall` | Remove what `delega plugin install` installed, from the hosts and from disk. |
| `delega schema request\|result` | Print the contracts built into this executable. |
| `delega skill list\|show NAME` | List the skills built into this executable, or print one. See [Skills](#skills). |
| `delega doctor` | Report providers found, their versions, limits in force and anything that would stop a run; `--output text\|json`. See [Doctor](#doctor). |
| `delega version` | Print the version; `--output json` prints the build identity (name, version, `schema_version`, revision, features and each skill's name, URI and digest), the same document as `delega://identity`. |

`delega skill show` with an unknown name lists the known names on standard error
and exits 2. `delega skill show NAME --output json` adds `content` to the name,
description, URI and digest that `delega skill list --output json` gives.

`delega run` options: `--request FILE`, `--provider`, `-p/--prompt`, `--stdin`,
`-C/--cwd`, `--name`, `--model`, `--effort`, `--timeout`, `--read`, `--write`,
`--full`, `--strict`, `--allow-command CMD` (repeatable), `--add-dir DIR` (repeatable),
`-e/--env NAME[=VALUE]` (repeatable), `--schema FILE`, `--resume ID`, `--ephemeral`,
`--output text|json`, `--dry-run`, `--progress`, and provider options after `--`.
`--name` is checked but does not appear in the result, because `delega run`
creates no worker; it matters for MCP and the library.

Behavior:

- **Timeout.** `--timeout` accepts `90s`, `20m`, `2h` or a plain number of
  seconds, and the CLI converts it to the request's `timeout_secs`. A value
  outside 1 to 86400 seconds (`0`, `25h`) is refused `invalid_request`, not a
  usage error.
- **Claude Code settings.** A Claude worker starts with settings sources off, so
  it inherits no hooks, `env`, `apiKeyHelper`, plugins, skills, MCP servers or
  instructions from the user's or the project's Claude Code settings, and with
  `CLAUDE_CODE_DISABLE_CLAUDE_MDS=1` and `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`, so
  Claude Code loads no `CLAUDE.md` file and no auto memory from the user's home
  directory into its context by itself (the worker can still read files with
  its tools). It does get
  their `permissions.deny` rules: delega reads the user's settings file
  (`settings.json` in the worker's `CLAUDE_CONFIG_DIR`, else
  `.claude/settings.json` under the worker's `HOME`) and the working directory's
  `.claude/settings.json` and `settings.local.json` (the latter also from the
  root of the git repository and, when that root is a linked worktree, from the
  main checkout), takes
  only that list, and adds the rules to the worker, where they can only narrow
  the run (also in `full`). A settings file that exists and cannot be used
  (unreadable, not a regular file, not JSON, a wrong type, over a limit) refuses the run with kind
  `settings_unreadable` or `deny_rules_over_limit`, naming the file. The result's
  `provider_notes` say how many rules were applied and from which files, and
  `--dry-run` lists the files and, under `user_deny_rules`, the rules. Codex
  gets none of this. See [providers.md](providers.md#deny-rules).
- **Environment.** A worker does not inherit the caller's environment. It
  receives a fixed base (`PATH`, the user, shell, locale, terminal, time zone,
  temporary and base directory variables, and the switches `NO_COLOR` and `CI`),
  proxy and certificate variables, and the provider's own
  variables, and nothing else; the lists are in
  [providers.md](providers.md#environment-policy). `--env NAME` (short `-e`)
  passes the caller's value of one more variable; `--env NAME=VALUE` sets it to
  VALUE, which may be empty and may contain `=`. Repeat the flag for more.
  Prefer the `NAME` form for secrets, because a value on the command line is
  visible in the process table and in shell history. A `NAME` that is not set
  in the caller's environment is ignored. A name that is not valid (letters,
  digits and underscore, not starting with a digit), a calling-session variable,
  any `DELEGA_*` variable and a name delega itself sets for the provider (such
  as `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS`) are refused before the run starts, in either form,
  with error kind `invalid_env_name` or `forbidden_env_name`; the message gives
  the position of an invalid name and never repeats it, so a value is not
  echoed. A variable whose name looks like a secret is allowed and adds a
  `provider_notes` entry that names it, for a passed variable and for one given
  a value. Every result of a run that passed the checks before start carries
  one note with the number of variables withheld and passed. `--env` is caller-side: a profile file does not carry it, and an
  `env` or `pass_env` field in one is refused as an unknown field. `delega mcp`
  takes the same flag, parsed the same way and applied to every worker the
  server starts; the refusals are the same, with the same kinds and messages.
  `delega mcp` checks every `--env` when it starts: a bad one is written to
  standard error as `delega mcp: refused: <kind>: <message>`, nothing is served
  and the status is 2. A name that only one provider's adapter owns (such as
  `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS`) is not known to be a problem until a
  worker for that provider starts, and is refused then, as a refused run.
  The [configuration file](#configuration-file)'s `[env]` section adds names
  and values the same way, with the same rules and refusals, and both apply: the
  file's `pass` names and `set` variables come first and the flags' after them,
  so a flag that sets the same variable wins.
- **Building the request.** The profile named by `--request` is read first and
  every flag that was given replaces the matching field; a flag that was not
  given leaves the profile's value. Lists (`--allow-command`, `--add-dir`,
  provider options after `--`) replace the profile's list, they are not added
  to it. `--read`, `--write` and `--full` replace `permissions.mode`;
  `--resume` replaces `session.ephemeral` and `--ephemeral` replaces
  `session.resume`. The prompt comes from `-p` or from `--stdin`, which reads
  standard input as UTF-8 up to the prompt limit; giving both is a refusal, not
  a usage error. `--schema FILE` is read as JSON and becomes `answer_schema`.
  `instructions` has no flag; give it in a profile.
  The profile and the schema file must be regular files; a device or a pipe is
  refused, and so is a file over the limit (5.25 MiB for a profile, enough for
  the largest prompt and instructions; 1 MiB for a schema file), with the
  limit named. A prompt that starts with a hyphen is given as `--prompt=-text` (or `-p=-text`); `-p -text` is a usage error, so `-p` never swallows the flag after it. Every relative path
  resolves against the caller's working directory: `--request`, `--schema`,
  `-C`, `--add-dir`, and `cwd` and `permissions.directories` in a profile. (The
  provider layer would otherwise resolve a relative extra directory against the
  worker's own working directory, so the CLI makes the paths absolute first.)
- **Output.** `--output text` (the default) prints the answer text to standard
  output, with a newline added when it lacks one, and nothing else there, and
  only when the outcome is `completed`; an empty answer prints nothing. For any other outcome nothing is printed
  on standard output, whatever partial text the provider produced; that text is
  in `text` of `--output json`. The outcome and the error of a run that did not
  complete go to standard error as one line, `delega run: <outcome>: <error.kind>: <error.message>`. Each denied
  request goes there as `delega run: denied: <request>`, whatever the outcome,
  and cleanup that could not be confirmed as `delega run: cleanup not
  confirmed: ...`. `--output json` prints the whole result as one
  pretty-printed JSON document and nothing else on standard output.
- **Exit status** follows the outcome: 0 `completed`, 1 `failed`, 2 `refused`
  (including usage errors), 124 `timed_out`, 130 `interrupted`. With
  `--output json` the result is always printed, whatever the exit status. A
  closed pipe on standard output (`delega run ... | head -1`) is not an error and
  does not change the status; any other failure to write the result, such as a
  standard output that is not open for writing, is reported on standard error
  and makes a completed run exit 1.
- **Refusal before a run exists.** A [configuration file](#configuration-file)
  that cannot be used comes first: it is read before the profile and the flags,
  and a refusal for it is printed as a `refused` result with its own kind
  (`config_unreadable`, `config_invalid`, or an environment kind), the status 2,
  and, with `--progress`, the `finished: refused` line, whatever else is wrong
  with the request. The result then carries the provider and the mode the flags named,
  and the time limit of `--timeout` when it is within 1 to 86400 seconds and the
  default (600) otherwise, because the file is what could not be
  read. An unreadable or invalid profile, a missing
  prompt or provider, `--prompt` together with `--stdin`, an unreadable or
  malformed schema file, and any other validation problem are printed as a
  `refused` result, not as a usage error: with `--output json` the result has
  `error.kind` `invalid_request` and the message lists every problem found, as
  `field: problem` separated by `; `; with text output each problem is a line
  on standard error, `delega run: refused: <field>: <problem>`, and standard
  output stays empty. The exit status is 2 in both. Such a result has no
  `worker`, no `started_at` and a confirmed `cleanup`, and the CLI makes its run
  id. Only an argument clap rejects (an unknown flag, two of `--read`, `--write`
  and `--full`, `--resume` with `--ephemeral`, a malformed `--timeout`) is a
  plain usage error with its own message and no result. If `delega` cannot
  start its async runtime or install the signal handlers, it prints a `refused`
  result with kind `runtime_unavailable` or `signals_unavailable` and starts
  nothing.
- **Interrupt.** The worker runs in a session of its own, so a terminal Ctrl-C
  or a hangup when the terminal closes reaches only `delega`. SIGINT, SIGTERM,
  SIGHUP and SIGQUIT to `delega` are handled alike: `delega` interrupts the run
  first, before writing anything, then waits for the worker and its descendants
  to be stopped, prints the result, which carries the cleanup verdict, and
  exits. The exit status follows the outcome, so it is 130 unless the provider
  had already finished when the signal came. The first signal starts the stop.
  Signals that arrive while it goes on are acknowledged once on standard error
  and otherwise ignored, so a second Ctrl-C cannot end `delega` while processes
  are still being stopped. The wait is bounded by the process layer's cleanup
  deadline (10 s by default), after which the result says what could not be confirmed. A signal
  during `--dry-run`, or before the handlers are installed at startup, has no
  worker to stop and takes the default action. **SIGKILL cannot be handled**:
  if `delega` is killed with it, or crashes without running its exit code, the
  worker keeps running and nothing enforces its time limit. Stop `delega` with
  SIGTERM or SIGINT, not SIGKILL.
- **Writing the result waits on the reader.** After the worker is stopped,
  `delega` writes the result to standard output on a thread of its own, and that
  write waits for whoever reads standard output; a large result and a reader that
  does not read can hold it indefinitely. The signal handlers stay installed
  during the write. A SIGINT, SIGTERM, SIGHUP or SIGQUIT asks the write to
  stop, and `delega` exits once the write has said how it ended. The worker is
  already gone by then.
  - **The exit status follows what was written.** The result goes out in pieces
    of at most `PIPE_BUF` bytes (512 on macOS, 4096 on Linux), each written
    only once `poll` says standard output is writable, and a stop is checked
    before each piece. If the print thread reports that the whole result was
    written, the exit status is the one the result calls for (0 for
    `completed`). If it reports that the write stopped with bytes left, the
    exit status is the outcome's, or 130 when the outcome was `completed`,
    because its result was not delivered; standard output then holds the start
    of the document. If it does not report within 1 s, the exit status is the
    cut-short one. `delega` does not change the mode of standard output: its
    file description can be shared with other processes, so a blocking
    descriptor stays blocking (`O_NONBLOCK` is never set), and a descriptor the
    caller made non-blocking stays non-blocking. An interrupted write
    (`EINTR`) is retried, a reader that closed the pipe (`EPIPE`) counts as
    delivered, and any other error, `EAGAIN` from a descriptor another process
    set to non-blocking included, is a write failure.
  - **Where the status can be wrong.** The status matches what was delivered
    except in these cases, all of which end in the cut-short status after the
    1 s wait:
    - **No answer in time after a complete write.** The last piece can reach
      standard output before the print thread records that the write finished.
      If that thread is then not scheduled for more than 1 s (the process is
      stopped or paused, for example), `delega` exits with the cut-short status
      after a complete result. A review reproduced this with an added 1.5 s
      delay at that point.
    - **A piece that blocks after `poll`.** The 20 ms poll interval bounds only
      the wait for standard output to become writable. A write after `poll`
      reported it writable can still block: when another process writes to the
      same pipe; on macOS when pipe memory is over its growth cap, because the
      kernel's writability check counts the larger of the default pipe capacity
      and the capacity currently allocated, which can exceed the room left; on a terminal with flow control,
      whose readiness does not promise room for a whole piece; and on a regular
      file on a stalled filesystem. A stop then waits the 1 s and exits with
      the cut-short status. A blocked piece of at most `PIPE_BUF` bytes goes
      into a pipe whole or not at all, so the status is wrong only if that
      piece completes the result as `delega` exits.
  - **How long it takes.** When no piece blocks, the write stops within 20 ms
    (one poll interval) and one piece. `delega` waits at most 1 s for the print
    thread to answer (it may still be building the document, or be in one of
    the cases above), then up to 1 s for standard error to take its last lines.
    Shutting down the async runtime waits at most 5 s more, and only for a
    helper thread that is stuck; none was in these measurements. Measured on
    macOS with the fake providers, from the signal to the exit with standard
    output stalled by a full pipe: 30 to 48 ms when standard error is read,
    1.02 to 1.04 s when standard error is a full pipe too (5 runs each).
  - **After an earlier signal.** If a signal had already been received during
    the run (it was used to stop the worker), the write is given 3 s and is
    then stopped the same way. Measured from the start of the write: 3 s plus
    under 20 ms (5 runs: 4.64 to 4.66 s from the signal, of which 1.64 to 1.67 s
    stopping the worker).
  - **Cost when nothing goes wrong.** Writing in pieces after `poll` costs time
    on the normal path. Measured in a review on an Apple M4 Pro, release
    builds, 20 alternated runs per revision, a result of about 3 MB drained
    through a pipe, median time from the first to the last byte: 0.73 ms before
    this write and 5.59 ms with it for Claude, 0.75 ms and 5.72 ms for Codex.
- **Standard error never holds up the run.** Everything `delega run` writes to
  standard error (progress, summaries, problems, signal acknowledgements) goes
  through one writer thread fed by a bounded queue (1024 lines). A line that
  finds the queue full is dropped and counted. The count also includes chunks of provider output that
  could not be handed to the progress consumer fast enough. If it is not zero,
  the last line on standard error is `progress: N events or output chunks were
  dropped: ...`, and with `--output json` a `provider_notes` entry in the result
  names the chunks lost at the provider output (not those lost on standard
  error, which the result cannot know). The line is written directly by the
  writer, so it cannot itself be dropped, and appears if standard error becomes
  writable before `delega` exits. Stopping the worker, the result on
  standard output and the exit never wait for standard error: at the end
  `delega` waits at most one second for the queue to be written and then
  exits. A caller that does not read standard error can therefore lose lines
  but not the result.
- **Dry run.** `--dry-run` validates the merged request and prints the provider
  executable, arguments, working directory and applied permissions without
  starting anything, not even a version probe. Text output is a readable list.
  JSON output is `{"schema_version": 1, "dry_run": {...}}` where the object is
  the provider layer's `DryRun`: provider, program, args, `env_names`, `env`,
  cwd, `time_limit_secs`, `stdin_bytes`, the permission `mapping`, `notes`,
  `before_start` and `applied`, the same object a result carries
  ([`applied`](#applied); the text output prints its configuration file and
  sources). `env` groups the names that would be passed by reason
  (`base`, `network`, `provider`, `named`, `set_by_caller`, `set_by_delega`),
  lists names given as `--env NAME` that are not set (`named_not_set`) and gives only the count of
  the caller's variables that are withheld (`withheld_count`); text output
  prints the same groups. It lists environment variable names, never values,
  never the names of withheld variables, and not the assignment, instructions or settings text, which are replaced by their
  size. `deny_rules` is always present and empty for Codex; for Claude it holds the settings files read with their rule
  counts, any local settings files ignored with the reason (`skipped`: a root that is the home directory, an entry that is not owned by the current user, or, in a repository root, a `.git` link or a `.claude` or settings-file link that is not the user's own in a directory only the user can write, with a target the user owns; anything the user does not own is skipped whatever its kind or mode; a link the user owns whose target cannot be opened or examined, such as one into a directory the user cannot search or through a regular file, is not skipped but refuses the run with `settings_unreadable`, because the owner cannot be known and a broken target of the user's own link is theirs to fix), `applied`, and the user's own deny rules under `user_deny_rules`
  (bounded, with `omitted`); a main checkout that could not be identified is a note, not a `skipped` entry; the launch arguments show a count in their place.
  A request that would be refused prints the refusal as for a run (a
  `refused` result in JSON, a line on standard error in text) and exits 2.
  A dry run exits 0 otherwise.
- **Progress.** `--progress` writes one line per provider event to standard
  error while the run goes on, and never to standard output. Lines start with
  `progress: `: `started <provider> (<executable>)`, then one line per
  provider output line as `<provider event type>: <summary>`, then `denied:
  <request>` for each request the result lists as denied, then `finished:
  <outcome>`. Denials come after the provider ends because Claude Code reports
  them only in its final event. What a summary holds: for an agent message, its
  length in characters (`message: 120 chars`) and no text; for a tool call or an
  item, the tool name, or the item type and status, and no command; for an
  error event, at most 80 characters of its text with credentials removed,
  which is free text the provider chose to write. A `denied:` line carries the
  denied request as `permissions.denied` lists it, so it can name a command or
  a path. `finished` is always the last `progress:` line (only the dropped-count
  line can follow it), also for a request refused before it starts, including
  one refused while it is built. With `--dry-run`, `--progress` writes nothing
  unless the request cannot be built. **Later:** due's trace files, heartbeats and
  summaries.
- **Provider executable.** The CLI honours `DELEGA_CLAUDE_BIN` and
  `DELEGA_CODEX_BIN` in its own environment, as the provider layer does
  ([providers.md](providers.md#finding-the-executable)), and the executables
  the [configuration file](#configuration-file) names, which the variables
  override. There is no flag for it. `--allow-command` for Claude Code is enforced by this executable's own
  hidden `command-guard` subcommand, which Claude Code starts before a `Bash`
  call; it is not part of the public surface.
- **Test-only variable.** `DELEGA_TEST_NO_PROCESS_LISTING`, when set to any value, makes process inspection unavailable so `cleanup` is reported as not confirmed; it is for the acceptance harness, can only make the verdict weaker, and is not part of the public surface.
- **Scope.** The CLI runs in the foreground only. It does not list or stop runs
  started by another process; supervising several workers is done over MCP.

### Doctor

`delega doctor` reports what a run would meet, without starting a worker and
without writing anything. It has no `--cwd` or `--env`: the checks are those of a run in the current
directory with no `--env`, with what the [configuration file](#configuration-file)
adds (its environment names and values and its executables), because a run
would have them. It checks both providers,
with the lookup, the version probe, the environment allowlist and the deny-rule
reader that `run` uses, so the answer is the one a run would get. The only
provider process it starts is the `--version` probe, with `run`'s five-second
limit and the environment a worker would get. It never prints an environment
value, a deny rule or the content of a file.

It reports, for `claude` and `codex` each:

- the executable a run would use and where it came from (`DELEGA_CLAUDE_BIN` or
  `DELEGA_CODEX_BIN`, `config` for the configuration file, or `PATH`; a library
  caller that sets `ProviderOptions::executable` gets `option`);
- the version from the probe, and for Claude Code whether the command hook
  interface is available (2.1.325 or later, which `--allow-command` needs); Codex
  has no minimum;
- how many environment variables a worker would receive, by reason (base, network,
  provider, set by delega) and how many of the caller's would be withheld, as
  counts only;
- for Claude Code, the deny rules a run would apply: the settings files read with
  their rule counts, the local files ignored and why, and the number applied.

It also reports the configuration file: the path it looked at and what chose it
(`DELEGA_CONFIG`, `XDG_CONFIG_HOME` or `HOME`; null when there is nowhere to
look), whether something is there (`found`), whether it was read and is in
effect (`read`), the dotted names of the keys it sets (`settings`, such as
`defaults.model`) and how many names `[env] pass` lists and variables `[env.set]`
sets, never a value.

It also reports the limits in force, read from the constants the code uses and,
for the supervisor, with the `[mcp]` limits of the configuration file applied: for a
run, the default and the range of the time limit, the output bounds and the
version probe limit; for the supervisor (see
[supervisor.md](supervisor.md#limits-and-defaults)), the worker, run and queue
limits, retention by count, size and time, the default wait and its ceiling, and
the cap on outstanding request checks; the skills built into this
executable with their digests, as `delega version --output json` lists them; and
whether process inspection works, by running one `ps` listing as cleanup does.

**Problems.** Each problem has a stable `kind`, a message, the provider it is
about (null when it is about the whole system) and what it `blocks`: `run`,
`command_grants`, `cleanup`, or `nothing` (a warning: no run is refused). Doctor's
verdicts always match what `run` does, including runs that would fail to start,
not only its refusals: a problem blocks `run` only where a run is refused or
fails for it.

| Kind | Blocks | Meaning |
| --- | --- | --- |
| `provider_not_startable` | `run` | The executable passed the lookup but cannot be started, such as a script whose interpreter is missing. A run fails to start with it (`spawn_failed` for Claude, `provider_preparation_failed` for Codex). |
| `provider_not_found` | `run` | No executable by the lookup, or the override names none. |
| `version_probe_failed` | `command_grants` for Claude, `nothing` for Codex | `--version` exited with an error, or was ended by a signal. A run records no version and goes on; only a Claude run that grants exact commands is refused. |
| `version_probe_timed_out` | `command_grants` for Claude, `nothing` for Codex | `--version` did not answer in five seconds. Same effect as a failed probe. |
| `unsupported_provider_version` | `command_grants` | Claude Code is older than 2.1.325 or its version could not be read, so a run with `--allow-command` is refused. Other runs go ahead. |
| `settings_unreadable` | `run` | A Claude Code settings file in the user's or the current directory's places exists and cannot be used, so every Claude run here is refused. The message names the file. |
| `deny_rules_over_limit` | `run` | The deny rules are over a bound, so every Claude run here is refused. |
| `config_unreadable` | `run` | The configuration file exists and cannot be used as a file (unreadable, not a regular file, owned by another user, writable by group or others, a link that is not trusted, over 1 MiB), or `DELEGA_CONFIG` names a file that does not exist or a path that is not absolute. About the whole system, so every run is stopped. Nothing from the file is applied to the rest of the report. |
| `config_invalid` | `run` | The file is not UTF-8 or TOML, has an unknown key, a value of the wrong type or outside its bound, or a path that is not absolute. The message names the key or the line. |
| `invalid_env_name`, `forbidden_env_name` | `run` | A name in the file's `[env]` that `--env` would refuse for the same reason. |
| `process_inspection_unavailable` | `cleanup` | The process listing failed, so every cleanup would be reported as not confirmed. Runs still start. |

Another refusal a run would give while it is built, such as a settings argument
over its bound, is reported under its own kind and blocks `run`. A provider is
`ready` when no problem blocks `run` for it.

**Output.** Text is the default; control characters anywhere in the text report (a provider's version line, paths, messages) are shown as `?`. `--output json` prints one versioned object
(`schema_version` 1) with `ready`, `version`, `cwd`, `providers`,
`process_inspection`, `config`, `limits`, `problems` and `skills`; its JSON Schema is
`schemas/doctor.schema.json`, generated from the types, checked by a test and
updated like the other schemas with `DELEGA_UPDATE_SCHEMAS=1`. Keys are in
alphabetical order. `env` and `deny_rules` are null when the launch could not be
built, and `deny_rules` and `command_hook` are null for Codex.

**Exit status.** 0 when a run can go ahead: at least one provider is ready,
process inspection works and the configuration file, if there is one, can be
used. 1 when no provider is ready, when the configuration file is refused (a
problem about the whole system, kind `config_*`, that blocks `run`; the rest of
the report is still printed, as if there were no file), or when process
inspection is unavailable (runs still start, but no cleanup can be confirmed). 2 when doctor itself cannot run,
such as an unusable current directory. A provider that is not installed is a
problem for that provider and does not by itself make the exit 1 when the other
provider is ready; read `problems` for it. A problem that blocks only
`command_grants` does not change the exit status either.

### Install and uninstall

`delega plugin install` installs a plugin that carries the delega skills and registers
`delega mcp` as the plugin's MCP server, into Claude Code, Codex or both.
`delega plugin uninstall` removes it. Neither starts a server or calls a model, and
neither edits a host's configuration files: a host is registered and unregistered
only by running its own CLI.

```sh
delega plugin install --dry-run         # print the files and the host commands
delega plugin install                   # every host found
delega plugin install --host claude     # Claude Code only
delega plugin uninstall --host codex    # remove from Codex, keep the directory for Claude Code
delega plugin uninstall                 # remove from every recorded host, then the directory
```

Options, for both commands: `--host claude|codex` (repeatable),
`--install-dir DIR`, `--dry-run` and `--output text|json`.

- **Managed directory.** `--install-dir DIR`, else `DELEGA_INSTALL_HOME`, else
  `$HOME/.local/share/delega`. It must be an absolute path. An existing
  directory is reused if it holds a valid marker or is empty. Any other
  non-empty directory is refused and left untouched.
- **Hosts.** The executables are found as `delega run` finds the providers:
  `DELEGA_CLAUDE_BIN` and `DELEGA_CODEX_BIN` (an absolute path), else `claude` and
  `codex` on the absolute entries of `PATH`. With `--host`, every named host must
  be found, or `delega plugin install` stops before writing anything. Without
  `--host`, missing hosts are reported and skipped, and at least one must be found.
- **Files written.** Under the managed directory:

  | Path | Content |
  | --- | --- |
  | `plugin/delega/bin/delega` | A copy of the running executable, mode 0755. |
  | `plugin/delega/skills/<name>/SKILL.md` | One per embedded skill, the bytes of `delega skill show NAME`. |
  | `plugin/delega/IDENTITY.json` | The document `delega version --output json` prints. |
  | `plugin/delega/.claude-plugin/plugin.json`, `plugin/delega/.mcp.json` | The Claude Code plugin and MCP manifests. The server is `${CLAUDE_PLUGIN_ROOT}/bin/delega` with the argument `mcp`, and no `cwd`. |
  | `plugin/delega/plugin.json`, `plugin/delega/mcp.json` | The Codex plugin and MCP manifests. The server is `./bin/delega` with the argument `mcp`, and no `cwd`. |
  | `.claude-plugin/marketplace.json`, `.agents/plugins/marketplace.json` | The local marketplace `delega-local`, one file per host. |
  | `.delega-install.json` | The ownership marker. |

- **Plugin version.** The delega version, a `+` and the lowercase hexadecimal SHA-256
  digest of the copied executable, for example `0.1.0-dev+3f9c...`. The digest is semver build
  metadata, so the version stays valid semver at a release. A host caches a plugin by version,
  so a changed executable, even another build of the same source revision, gets
  a new version and a refreshed cache. Running `delega plugin install` again
  with identical bytes gives the same version.
- **Marker.** `.delega-install.json`, format version 1: `format_version`,
  `managed_by` (`delega`), `version`, `revision`, `plugin_version`, `files` (every
  file written, relative to the directory, the marker itself excepted) and
  `hosts` (every host registered, `claude` and `codex`). Both commands refuse a
  marker that is not valid format 1: wrong `managed_by` or version, a host that
  is not `claude` or `codex` or is repeated, a file path that is absolute, has a
  `..` component or is repeated. `files` and `hosts` accumulate across repeated
  runs of `delega plugin install`.

Host commands, run from `/` with no input, each limited to 300 s (listings 30 s).
`ID` is `delega@delega-local` and `DIR` the managed directory.

| Host | `delega plugin install` | `delega plugin uninstall` |
| --- | --- | --- |
| Claude Code | `claude plugin marketplace add DIR`, `claude plugin install ID`, `claude plugin update ID --scope user` | `claude plugin uninstall ID --scope user`, `claude plugin marketplace remove delega-local --scope user` |
| Codex | `codex plugin marketplace add DIR`, `codex plugin add ID` | `codex plugin remove ID`, `codex plugin marketplace remove delega-local` |

`delega plugin install` runs the whole sequence every time. `plugin update`
refreshes Claude Code's cached copy, which a repeated `plugin install` alone
leaves stale. Codex's `plugin add` copies the plugin into its cache each time.
If a command fails, the rest of that host's sequence is skipped, the other
host is still installed, the files stay in place, and the status is 1. Fix the
error and run `delega plugin install` again. No host cache is deleted by delega.

`delega plugin uninstall` first reads each selected host's registrations with
`plugin marketplace list --json` and `plugin list --json`, from `/`, for every
host before it changes any. It removes the plugin and then the marketplace only
when both point at the managed directory, and stops, keeping every file, on:
an unmanaged directory, an invalid marker, `delega-local` registered from
another directory, a `delega@delega-local` registration that is not the
managed directory's, another plugin installed from `delega-local`, a listing
that failed, wrote diagnostics, was not the expected JSON or was ambiguous, or
a recorded host whose CLI cannot be found. Claude Code removal is limited to user
scope. A plugin or marketplace the host no longer has is not removed again. If a
removal command fails, the files are kept and the marker keeps that host. Without
`--host`, every host in the marker is removed. With `--host`, only the named
hosts are, and the directory is kept while the marker still lists another host.
The managed directory is removed only when no host remains in the marker, and
only the files the marker lists, so anything else in it keeps the directory. A
directory that does not exist is nothing to do and runs no host command.

`--dry-run` prints the files and the host commands and writes nothing and runs
nothing. A dry run of `delega plugin uninstall` skips the listings and
ownership verification.

The manifests set no `cwd`. A host decides the server's working directory: the
Agent Plugins specification makes it the plugin root when `cwd` is omitted, so on
Codex the server starts in the plugin's directory, and Claude Code does not
document it for plugin MCP servers. That is why `delega mcp` refuses a `start`
without an absolute `cwd` (`cwd_required`): the server's own directory is never
used as a worker's default.

Claude Code puts an enabled plugin's `bin/` directory on the Bash tool's `PATH`,
so `delega` becomes a bare command in Claude Code sessions. Claude.ai and Cowork
refuse to install a plugin that has a top-level `bin/`, and this plugin has one
(`plugin/delega/bin/`), so it cannot be installed there.

The installed plugin starts `delega mcp` with only the argument `mcp`, so its
workers get only the fixed environment allowlist and no `--env` names or values.

Cleanup and limits:

- **Removal by hand.** If the managed directory was deleted by hand, `delega plugin uninstall`
  reports it absent and runs no host command, because without the marker it cannot
  tell whose registration it would remove. Remove the registrations with the
  hosts' own commands, from a neutral directory: `claude plugin uninstall
  delega@delega-local --scope user` and `claude plugin marketplace remove
  delega-local --scope user`; `codex plugin remove delega@delega-local` and
  `codex plugin marketplace remove delega-local`.
- **Empty directory.** `delega plugin uninstall` removes the managed directory when it ends up
  empty, including an empty one chosen with `--install-dir`. It deletes only the
  files the marker lists, so other files keep the directory.
- **Marker paths.** A marker that names a path `delega plugin install` of this build cannot write
  (anything outside the marketplaces, the executable, the plugin's manifests,
  `IDENTITY.json` and `skills/<name>/SKILL.md` with a name of `a-z`, `0-9` and
  `-`) is refused as invalid, not deleted.
- **No lock.** Do not run `delega plugin install` or `delega plugin uninstall` concurrently on one directory;
  it is not supported.

After `delega plugin install`, reload each host to start the new server. In
Claude Code, start a new session or reconnect `plugin:delega:delega` from `/mcp`
(`/reload-plugins` re-reads the skills but does not restart the running
server); in Codex, start a new session. Then read `delega://identity` and check it
against `plugin/delega/IDENTITY.json`.

`--output json` prints one document, including for exit-1 failures. The
`operation` field is `install` or `uninstall`. Both documents have `ok`,
`dry_run`, `install_dir`, `hosts` (each with its `status`, the `commands` run
and the `reload` step) and `error`. Tests validate the documents against
[`schemas/plugin-install.schema.json`](../schemas/plugin-install.schema.json) and
[`schemas/plugin-uninstall.schema.json`](../schemas/plugin-uninstall.schema.json).

Exit status: 0 on success, including a dry run; 1 when a host command failed or
something was refused (a host not found, a foreign directory, an invalid marker,
a foreign registration); 2 for a usage error, including a relative directory and
no `HOME` without `--install-dir` or `DELEGA_INSTALL_HOME`. Standard error says
what was refused.

## Configuration file

One optional TOML file holds what a person wants every `delega run`, `delega
mcp` and `delega doctor` to start from. With no file, behaviour is exactly what
it is without this section. Its schema is `schemas/config.schema.json`,
generated from the Rust types, checked by a test and updated like the other
schemas with `DELEGA_UPDATE_SCHEMAS=1`.

```toml
schema_version = 1

[defaults]
provider = "claude"
model = "sonnet"
effort = "medium"
timeout_secs = 1200

[defaults.permissions]
mode = "read"
strict = false

[env]
pass = ["AWS_SESSION_TOKEN", "TOOLCHAIN_HOME"]

[env.set]
AWS_REGION = "eu-west-1"

[providers.claude]
executable = "/opt/claude/bin/claude"

[providers.codex]
executable = "/opt/codex/bin/codex"

[mcp]
max_live_workers = 8
max_running_runs = 2
max_queued_per_worker = 4
wait_default_secs = 20
wait_ceiling_secs = 120
```

**Location.** `DELEGA_CONFIG`, when set and not empty, names the file and must be
an absolute path. Otherwise `$XDG_CONFIG_HOME/delega/config.toml` when
`XDG_CONFIG_HOME` is set and absolute (a relative value is ignored, as the XDG
specification says), otherwise `$HOME/.config/delega/config.toml`; with no
absolute `HOME` either there is nowhere to look and nothing is configured. No
file in a repository or in the working directory is ever read. A missing file at
either default place is no configuration. A missing file that `DELEGA_CONFIG`
names is refused, because the variable was set on purpose. The file is read when
a command starts; `delega mcp` reads it once, so a change reaches a server on
its next start.

**Format.** `schema_version` is required and must be 1. Every key is optional
otherwise, and an unknown key is refused at any level, naming it (such as
`mcp.max_workers`). The sections:

| Key | Value |
| --- | --- |
| `[defaults]` `provider`, `model`, `effort`, `timeout_secs` | As the request fields of the same name ([The request](#the-request)), with the same bounds: `timeout_secs` 1 to 86400, `model` not empty and not starting with `-`. `prompt` and `cwd` are refused with a message that says why: they belong to one request, not to a default. |
| `[defaults.permissions]` `mode`, `strict` | As `permissions.mode` and `permissions.strict`. |
| `[env]` `pass` | A list of names, as `--env NAME`. |
| `[env.set]` | Names set to string values, as `--env NAME=VALUE`. A value is never printed, in a result, a report, a message or a debug text. |
| `[providers.claude]`, `[providers.codex]` `executable` | An absolute path. `DELEGA_CLAUDE_BIN` or `DELEGA_CODEX_BIN` overrides it; it comes before `PATH` ([providers.md](providers.md#finding-the-executable)). |
| `[mcp]` | The supervisor's limits, each bounded by the constant that is its default today, so the file can lower a limit and cannot raise one: `max_live_workers` 1 to 16, `max_running_runs` 1 to 4, `max_queued_per_worker` 0 to 16, `wait_ceiling_secs` 1 to 300 and `wait_default_secs` 0 to 300 and not more than the ceiling in force (the file's, or 300). A file that lowers only the ceiling lowers the default with it, so `list` and `doctor` report what a wait applies. Only `delega mcp` uses them; `delega doctor` shows them. |

The names in `[env]` follow the rules and have the refusals of `--env` ([Command
line](#command-line)): an invalid name is `invalid_env_name`, a calling-session,
`DELEGA_*` or adapter-owned name is `forbidden_env_name`, a secret-looking name
is allowed and noted in `provider_notes` when a run passes it. The position in
the message counts the entries of `pass` in the file's order and the keys of
`set` in alphabetical order, and an invalid name is not repeated.

**Precedence.** Built-in defaults, then the file, then a profile (`--request
FILE`, or `profile` on MCP `start`), then explicit flags or tool arguments, each
replacing the fields the one before set; a list, such as `permissions.commands`,
replaces and is not added to. A flag turns `--strict` on only, so a file's
`strict = true` stays on for `delega run` unless a profile sets `strict = false`;
over MCP a call can pass `permissions.strict: false`. For `[env]` the file's
names and values are added to the `--env` names of the run or the server: both
apply, the file's first, so a flag that sets the same variable wins. `DELEGA_*`
variables override only executables (`DELEGA_CLAUDE_BIN`, `DELEGA_CODEX_BIN`) and
the location (`DELEGA_CONFIG`); no other setting has a variable.

**Safety.** The file is used only when it is a regular file owned by the current
user and not writable by group or others, and at most 1 MiB, and the directory
that holds it is owned by the user and not writable by group or others (link or
not, because whoever can write the directory can swap the file or plant a link
or a hard link). The entry is looked at without following a link first: a
device, a pipe, a directory or a socket is refused from that result, before it is
opened, because opening some of them has effects. A regular file is then opened
without following a link and without blocking, and the owner, the type and the
mode are checked again on the open handle, so a swap between the look and the
open is caught; a link swapped in is refused. A symbolic link at the file, as a dotfiles manager makes, is followed only when the user owns the link
and the directory that holds it, that directory is not writable by group or
others, and the target passes the same checks; a link to nothing is refused. This
is the rule `delega` applies to a link in a repository root when it reads Claude
Code settings, except that a failing link is refused and not skipped, because a
skipped file would silently drop what the user set.

**Refusals.** A file that cannot be used is never ignored. Every command that
reads it refuses, with a kind that names the problem, the path, and the key or the
line, and never a value from the file:

| Kind | When |
| --- | --- |
| `config_unreadable` | It cannot be used as a file: it cannot be opened or read, is not a regular file, is owned by another user, is writable by group or others (the message gives the mode), is in a directory that is not the user's or that group or others can write, is a link that fails the rule above, is over 1 MiB, or `DELEGA_CONFIG` names a file that does not exist or a path that is not absolute. |
| `config_invalid` | It is not UTF-8 or TOML (the message gives the line and column, not the parser's text, which can quote the file), has an unknown key, a value of the wrong type or outside its bound (the message names the key and the bound, or the accepted names for an enumeration), a path that is not absolute, or `mcp.wait_default_secs` above the ceiling. At most ten problems are listed. |
| `invalid_env_name`, `forbidden_env_name` | A name in `[env]` that `--env` would refuse for the same reason. |

For `delega run` the file is read first, before the profile and the flags, because
the request is built from it, so it is the first refusal there is: a request with
no prompt and a file that cannot be used is refused for the file. It is printed
as a `refused` result like any refusal before a run exists (JSON with `--output
json`, a line `delega run: refused: <kind>: <message>` in text, status 2), also
under `--dry-run`. `delega mcp` refuses to start: nothing is served, the line on
standard error is `delega mcp: refused: <kind>: <message>` and the status is 2.
`delega doctor` reports it as a problem of the whole system that blocks `run`,
keeps reporting everything else as if there were no file, and exits 1
([Doctor](#doctor)).

**Where it is applied.** `delega run`: the request defaults, the environment and
the executables. `delega mcp`: the `[mcp]` limits (they become the supervisor's
limits), the `[defaults]` under every `start`, and the environment and
executables of every worker. `delega doctor`: the same environment and
executables, so its answer matches a run, the limits in force, and a report of
the file. The supervisor library's own API is unchanged except that its limits
have a default wait (`Limits::wait_default`); the command line layers read the
file and pass the values in. Every result and dry run reports the file as
`applied.config` (path and SHA-256) and the layer of each request field as
`applied.sources` ([`applied`](#applied)). The library gains
`Supervisor::start_with_origin` and `ProviderOptions::origin` for a caller that
builds a request from layers and wants them reported; without them `applied`
carries no `config` or `sources`.

## MCP

The MCP server is for a caller that supervises several workers at once: start
them, keep working, collect results. Seven tools in the first version, and an
eighth, `respond`, when live asking is added.

| Tool | Arguments | Returns |
| --- | --- | --- |
| `start` | The request fields, or `profile` plus overrides; an absolute `cwd` is required, from the call or the profile | After the request checks (at most 10 s): `run`, `worker` (absent when refused), `state` (`queued`, `running`, or `finished` for a refusal), `ahead` while queued, and `outcome` and `error` when refused. |
| `follow_up` | `worker`, `prompt`, optional `timeout_secs` | At once: the same fields as `start`, `worker` always present. Queues behind a running run. |
| `wait` | `runs` or `workers` (list), `timeout_secs`, `after` (cursor) | When any of them finishes, or on timeout: `timed_out`, the `timeout_secs` applied, the current state of each named run and worker (with `changed` when `after` was given), `unknown` ids and a `cursor`. Never an error on timeout. `released_by_shutdown: true` is present, and otherwise absent, when the server's shutdown released the wait: nothing named need have finished, and a named run can still be `running` while it is stopped, so `timed_out: false` alone does not mean something finished. |
| `read` | `run` or `worker`, `detail`, `after` (cursor), `text_offset` (characters) | The result if finished; otherwise state and `progress` (partial text, recent activity, times). `text_window` when `text_offset` was given, and a `cursor`. |
| `list` | none | Live workers, then closed ones whose results are still kept, each with state, name, provider, current run, queue length, last run and outcome, session id and `can_follow_up`; the limits in force (including the default and ceiling of `wait`) and in use; a `cursor`. |
| `interrupt` | `run` or `worker` | After the run has stopped and cleanup has been attempted, at most 30 s: `settled`, `was_queued` and the compact result, `interrupted` (or unchanged if the run had already finished). |
| `respond` (**Later**) | `interaction`, `decision` (`allow`, `deny`), optional `note` | Acknowledgement. Only with `on_request: ask`. |
| `close` | `worker` | Stops the worker's running run (at most 30 s), discards queued runs and frees its slot: `already_closed`, `discarded`, `stopped_run`, `settled` and the stopped run's compact result. Results already produced stay readable for the retention time. Closing again answers the same way. |

`start` requires an absolute `cwd`. The MCP server's own working directory is
chosen by the host (Codex starts a plugin's server in the plugin root), so it is
never a default: a merged request with no `cwd` or a relative one is refused as
a finished run, kind `cwd_required`, readable with `read`, with no worker and no
process. The input schema lists `cwd` as optional, because a `profile` may supply
it and a schema that required it would reject a call with only a `profile`; the
requirement is checked on the merged request. `follow_up` takes no `cwd`: it
runs in the first request's, which was absolute. `delega run` keeps defaulting to
the caller's current directory. Over MCP a relative `permissions.directories`
entry is relative to the call's `cwd`, never to the server's own working
directory, and a refusal names the resulting path. The check is `Supervisor::start_requiring_cwd`,
which only the MCP adapter calls.

Rules that hold for every tool:

- **Starting never waits for the run.** `start` returns once its request checks
  finish (at most 10 s); `follow_up` returns at once. Only `wait`, `interrupt`
  and `close` wait on a run, each bounded. `wait` is capped by the wait ceiling
  (300 s) and accepts `timeout_secs: 0`, which returns the current states at
  once; `interrupt` and `close` wait for the run to settle for at most 30 s,
  after which the response says it has not settled. A caller should keep each
  wait short and pass the cursor back instead of waiting long. These three
  limits are counted in awake time and also checked against wall time about
  once a second of awake time. After the machine slept past a limit, the
  waiting ends at the first wall check after the machine wakes, about one
  check interval of awake time later plus scheduling, unless the wall clock was
  set back meanwhile so that the check reads less than the limit; it does not
  end while the machine sleeps. A wall clock set forward ends the waiting early
  only if the elapsed wall time read reaches the limit while the call is still
  waiting, and one that reads earlier than at the start counts as no time
  passed. Whichever clock ends the waiting, the call then takes the same path
  as when its limit passes on the awake-time timer, so it answers the same way
  in both cases. A run's `timeout_secs` counts awake time only: a run paused by
  sleep has not used its time.
  [supervisor.md](supervisor.md#how-the-bounds-count-time) has the detail.
- **States** a worker can be in: `queued`, `running`, `idle`, `closed`, and
  **later** `waiting_for_answer`. A run is `queued`, `running`, or finished with
  an outcome. A worker is `queued` when nothing is running and a run waits for a
  running slot, `running` while a run goes (more may wait behind it), `idle` when
  nothing is running or queued, and `closed` for good after `close`. The
  transition tables are in [supervisor.md](supervisor.md#states).
- **Detail.** `read` is compact by default; `detail: "full"` adds every field of
  the result, the full activity history, progress for a finished run, and the
  supervisor's diagnostics for the run (provider, mode, times, why a stop was
  requested). Process diagnostics from the provider are not included.
  `interrupt` and `close` return compact results. A compact result includes
  `applied` ([`applied`](#applied)), because it is small; usage and provenance
  stay out of it.
- **Limits** are admission limits: 16 live workers, 4 running runs and 16 runs
  queued behind one worker by default, reported by `list` with what is in use,
  and by `delega doctor` as configured. The `[mcp]` section of the
  [configuration file](#configuration-file) can lower each of them, and the
  default and the ceiling of `wait`; `list` reports the values in force, with
  `default_wait_secs` beside `wait_ceiling_secs`. A closed worker frees its slot. Reaching a limit
  is a `refused` outcome with the limit named, not a dead end that needs a
  restart. The supervisor applies them so: a `start` beyond the live-worker limit
  is refused (`limit_reached`); a `follow_up` beyond the per-worker queue limit
  is refused (`queue_full`); a run beyond the running-runs limit is not refused,
  it stays `queued` until a slot frees. At most 16 request
  checks run at once; a `start` that finds them all busy waits for one, within the
  same 10 s bound that covers its check, and is refused `preflight_timeout` only
  if none frees in time, which happens when the filesystem is not answering.
  Defaults are in
  [supervisor.md](supervisor.md#limits-and-defaults).
- **Refusals are runs.** A `start` or `follow_up` that cannot go ahead returns a
  run id whose result is `refused`, and `read` returns it. A refused `start`
  creates no worker; it is refused for an invalid request (`invalid_request`), a
  name already used by a live worker (`name_in_use`), the live-worker limit
  (`limit_reached`), a missing or relative `cwd` (`cwd_required`), any refusal
  the provider layer finds before start, or checks that did not finish in 10 s
  (`preflight_timeout`). A `follow_up` is refused when the worker is closed
  (`worker_closed`), its queue is full (`queue_full`), it was started
  `ephemeral` or has no session, because none was given with `session.resume`
  and no run reported one (`no_session`), or the
  request is invalid (`invalid_request`). Once the server is stopping, `start`
  and `follow_up` are tool errors: "the supervisor was shut down and takes no
  new work". An id
  that names nothing is an error, not a run, for `follow_up`, `read`,
  `interrupt` and `close`. `wait` is the exception: it lists ids that name
  nothing in `unknown` and waits for the others, and a `wait` whose ids all name
  nothing returns at once with them in `unknown`, not as an error. A `wait`
  that names no run and no worker, or more than 256 ids, is a tool error.
- **Cursors** number every finished run and every activity entry in one
  sequence. The `after` argument of `wait` and `read` is that cursor, called
  `after_cursor` in the library. `wait` with a cursor returns when something
  named finished after it; `read` with a cursor returns only activity after it.
- **Long answers.** A compact `read` returns the first 20000 characters of the
  answer and says so, with the full length; `text_offset` reads the window that
  starts at that character, with the total and whether more follows. `detail:
  full` without `text_offset` returns the whole answer. A compact read leaves
  out a structured answer larger than 16 KiB serialized and says so
  (`structured_omitted`, `structured_bytes`). For a run that has not finished, a
  compact read returns the last 2000 characters of partial text and the last 10
  activity entries; the supervisor keeps at most 16384 characters and 200
  entries per run. Consecutive provider `system` events with the same summary
  are one entry, replaced by each new one with a new cursor, its summary ending
  in ` (N in a row)` from the second on, such as `thinking_tokens (37 in a
  row)`; `activity_total` counts every event. Page a long answer by run id. A `read` by worker
  returns the worker's latest run, and a follow-up that finishes between two
  pages makes the next page come from the new run; the response names the run, so
  take the run id from the first page and pass it on the later ones, and treat a
  different run id as a different answer.
- **Retention.** Finished results stay readable for a bounded count (256),
  estimated size (64 MiB, oldest dropped first, the newest always kept) and time
  (1 hour), after which their ids are unknown. Closing a worker discards its
  queued runs, each finished `interrupted` with a message that starts
  `discarded by close` (`discarded by shutdown` when the server stops).
- **Follow-ups.** A follow-up runs with the worker's first request (provider,
  `cwd`, permissions, model, effort, instructions, `answer_schema`,
  `native_args`), the new prompt, `timeout_secs` when given, and the worker's
  session: the one given with `session.resume` at start, or else the first one a
  run reported. A later run that reports another id does not replace it.
- **Ownership.** Workers belong to the connection that started them and stop
  when it closes.
- **History is data.** Text a worker returns is never an instruction to the
  caller.
- **Results.** Every tool returns its result as structured content and as the
  same JSON in a text block, and has an input schema and an output schema
  (committed under `schemas/mcp/`, one `<tool>.schema.json` per tool, each
  holding the tool's `name`, `inputSchema` and `outputSchema`; a test
  regenerates them with `DELEGA_UPDATE_SCHEMAS=1 cargo test -p delega-mcp
  committed_tool_schemas` and fails when they are stale). An argument a tool
  does not have is rejected. An id that names nothing, a `start` with no
  `provider` or `prompt`, a profile that cannot be read, a `read` or
  `interrupt` that gives neither or both of `run` and `worker`, and a `wait` with
  nothing to wait for are tool errors with a message. A `wait` with an unknown id
  is not (see above). Everything the supervisor refuses is a run, as above.
- **`start` arguments.** The request fields, all optional in the schema, and
  `profile`, the path of a profile file. The profile is read first and every
  other argument replaces the matching field, with the same code `delega run`
  uses (`delega_core::profile`): lists replace, `resume` and `ephemeral`
  exclude each other, and a relative `profile` path resolves against the server's
  working directory. The merged request must have an absolute `cwd`, or the start
  is refused `cwd_required`. A request that has a provider and a prompt but fails the contract
  (a blank prompt, for example) is a refused run, not a tool error. The input
  schema states the request schema's ranges and list counts (`timeout_secs`
  between 1 and 86400, at most 64 `native_args`, 256 `permissions.commands` and
  64 `permissions.directories`) from the same refinements, so a value outside
  them is rejected as an invalid argument before the request is built. String
  lengths are not in the schema; an over-long value is a refused run
  (`invalid_request`).
  The merged request starts from the `[defaults]` of the configuration file
  (see [Configuration file](#configuration-file)): the order is the file, then
  `profile`, then the arguments of the call, so a `start` that gives no
  `provider` uses the file's, and one that gives `timeout_secs` replaces it.
  There is no per-call environment argument. `follow_up.timeout_secs` has the
  same range in its schema. `profile` lets the client make the server read a
  path it can read, which is no more than `cwd` with `permissions.mode: full`
  already allows; whether a path exists and is a profile can be learned from the
  error. The error for a file that is not a valid profile gives the path, the
  kind of error and the line and column, and none of the file's text, field
  names or values (`delega run` prints the parser's full message, because the
  file is the caller's own).
- **Wait.** `timeout_secs` defaults to 30 (`mcp.wait_default_secs`) and is
  capped by the wait ceiling (300, `mcp.wait_ceiling_secs`). It is counted in
  awake time and checked against wall time, as the rule on starting says.
  `read` takes `text_offset`, a number of characters, to page a long answer;
  page by run id, because a worker's latest run can change between pages.

### Running the server

`delega mcp` serves one connection on standard input and output, with one
supervisor. Standard output carries protocol messages and nothing else.
Diagnostics (a ready line, a signal received, cleanup that could not be
confirmed) go to standard error through the writer `delega run` uses, which
never waits on standard error and takes its lock for one line at a time. The
ready line reads `delega mcp: <version> ready`. At most 128 calls are handled at
once, and one input message may be at most 32 MiB.

- **Configuration file.** The user-level [configuration file](#configuration-file)
  is read once, when the server starts. Its `[mcp]` limits become the
  supervisor's, its `[defaults]` are the lowest layer under every `start`, and
  its `[env]` and `[providers]` apply to every worker, as for `run`. A file that
  cannot be used refuses to start the server: nothing is served, the line on
  standard error is `delega mcp: refused: <kind>: <message>` and the status is 2.
  A change to the file takes effect when the server is restarted.
- **Provider executables and environment.** `DELEGA_CLAUDE_BIN` and
  `DELEGA_CODEX_BIN` are honoured as for `run`, and so is the rest that `run`
  reads for provider options: the command guard is this executable's own
  `command-guard` subcommand, so `permissions.commands` is enforced for Claude
  Code, and `DELEGA_TEST_NO_PROCESS_LISTING` reaches the process layer. `run`'s
  `--progress` has no counterpart: progress is `read`'s activity.
- **Stopping.** When standard input reaches end of file, or on SIGINT, SIGTERM,
  SIGHUP or SIGQUIT, the server stops reading, so no new call is accepted. It
  then calls `Supervisor::shutdown` and waits for it, bounded by the
  supervisor's own shutdown bound (30 s). Shutdown wakes every `wait` that is
  blocked, whatever its cursor, and answers each with the current states, so no
  waiting call is held to the transport's drain limit: a `wait` with a cursor that
  arrives after shutdown began answers at once. Each of those answers carries `released_by_shutdown: true` (an additive field; `schema_version` is unchanged), except one that a finished run released on its own account, which does not carry it. The server then waits for the transport at
  most 15 s (a 10 s drain and 5 s grace) and gives up on it, because a client
  that stopped reading standard output blocks the write inside the transport's
  read loop. Only then does the runtime drop and the process exit. The workers
  are stopped before that wait, never after it.
- **A client that stops reading.** If the client keeps the pipe open and stops
  reading standard output until it is full, the transport cannot read standard
  input, so end of file on standard input is not noticed and does not stop the
  workers; a signal does, and the server then exits after the bound above. There
  is no independent watch for end of file: the transport owns standard input, and
  a second reader would take bytes from it. A client that does this has stopped
  using the connection and should signal the server.
- **Exit status.** 0 after end of input; 130 after a signal, the status `run`
  gives an interrupted run, for all four signals; 1 when the workers could not
  be confirmed stopped (any run on the connection ended with its cleanup
  unconfirmed, including one that a later run on the same worker, such as a
  refused `follow_up`, replaced as the worker's latest, or a run had not
  settled) or the supervisor had failed, which wins over 0 and 130, or the
  transport failed after end of input; each is described on standard error.
  The supervisor records an unconfirmed cleanup when the run settles, not from
  the results retention still keeps, and remembers the 256 most recent. A
  transport that had to be given up on after a signal does not change 130. 2
  when the server could not start (a refused `--env`, a configuration file that
  cannot be used, no async runtime, no signal handlers). A signal during start-up, before the
  handlers are installed, takes its default action, and so does SIGKILL, which
  cannot be handled: the workers of a server killed that way keep running.
- **Second signal.** As in `run`, a signal that arrives while the workers are
  being stopped is acknowledged once on standard error and otherwise ignored, so
  a second Ctrl-C cannot end the server before the workers are stopped.

Two simplifications against the earlier projects. counterpart has two ways to
add work to a worker, `send_message` and `followup_task`; here there is one,
`follow_up`, which queues. due separates releasing a run from releasing a
session; here there is one `close`, and finished runs are kept for a bounded
time and count.

## Skills

delega carries its own guidance: short documents that tell a calling agent how
to use it well. They ship inside the executable, so the guidance always matches
the behavior installed, with no source checkout or network.

There is one source and three doors.

- **Source.** Each skill is a directory in the repository, `skills/<name>/`,
  holding a `SKILL.md` with name and description frontmatter. The build embeds
  every `SKILL.md` as one catalog and refuses a skill that does not parse.
  Nothing is read from disk at run time. Supporting files are not embedded yet.
- **CLI.** `delega skill list` prints names and descriptions, and
  `delega skill show NAME` prints one. `--output json` gives a versioned
  catalog a program can select from.
- **MCP.** The server offers the same catalog through the MCP Skills extension
  (SEP-2640): each skill is listed by `skills/list` and by `resources/list`, so
  a host without the extension can still read it as an ordinary resource, and
  is readable at `skill://delega/<name>/SKILL.md`, with the bytes of
  `delega skill show NAME`. The server also serves `delega://identity`, JSON
  with `name`, `version`, `schema_version` (the contract's), `revision` (a
  12-character source revision or `unknown`), `features` (empty: the crates
  define no Cargo features) and `skills`, the name, URI and SHA-256 digest of
  each skill; `delega version --output json` prints the same document.
- **Plugin.** `delega plugin install` installs the same files as a plugin into Claude Code
  and Codex, for hosts that load skills from disk
  ([Install and uninstall](#install-and-uninstall)).

Rules:

- **Same bytes everywhere.** The three doors serve identical content. The build
  identity (`delega version --output json`, and `delega://identity` over MCP)
  carries a digest of each skill, so a caller can tell which guidance it has.
- **Skills only describe what exists.** A test fails if a skill names a command,
  flag, tool or argument that this build does not offer. Every build includes
  the MCP server, so every build ships both skills.
- **Reading is not permission.** Reading a skill does not activate it, grant a
  tool, or allow a bundled script to run. That stays with the host.
- **No project policy.** Skills describe how to use delega. Rules that belong to
  one project or one person stay in that project's own instructions.

First skills, one per way of using delega:

| Skill | For |
| --- | --- |
| `run` | Calling `delega run` from a script or another agent: writing the assignment, choosing permissions and a time limit, reading the result, checking the work. |
| `delegate` | Supervising workers over MCP: start, wait, read, follow up, interrupt, close, and what each outcome means. |

More are added with the features they describe, such as a review skill with the
review layer.

## Library

The CLI and the MCP server are thin layers over one Rust API with the same
nouns: build a `Request`, `start` it on a `Supervisor` to get a worker id and a
run id, `wait` for the result, `read` it, `follow_up`, `interrupt`, `close`,
`list`, and `shutdown` to stop every worker.
Anything both surfaces compute lives in the library. `delega run` uses the
provider layer's `run` directly, for one assignment in the foreground; the
supervisor is for several at once.

A program that embeds the library and starts processes of its own on other
threads should hold `delega_core::process::spawn_guard()` across each of its
spawn calls. On macOS std marks a child's pipes close-on-exec only after
creating them, so without the guard a child of the program can inherit a
worker's pipes, or a worker the program's, and a worker that holds another's
standard input keeps it from ever reading end of input. delega takes the same
lock for every process it starts in-process: providers, version probes, `ps`
listings and the host commands of `delega plugin install` and `delega plugin uninstall`. Hold the guard only
across the spawn call, never while calling into delega or across an `.await`:
every run waits for it.

## Not in this surface

- **Review.** An enforced independent review of a worker's change (counterpart's
  pair) is a later layer with its own tools, built on the tools above. What it needs
  from this layer: a role and an owner on a worker, an event stream for an
  owner, and a barrier that stops new assignments to a worker. Those should be
  possible to add without changing the tools above.
- **Session history.** That is trovador.
- **A scripting harness and a JSON-RPC surface.** due has both; left out for
  now.

## Where each part comes from

| Part of this surface | due | counterpart | delega |
| --- | --- | --- | --- |
| `run` command, profiles, `--dry-run`, `--stdin` | `due run`, `--request`, `--dry-run` | none | Take due's. |
| Request and result as versioned JSON Schema | `run-request`, `run-result` schemas, `due schema` | Tool schemas in `contract/`, no versioning promise | Take due's shape; add `outcome`, `cleanup`, `permissions`, `provider_notes`. |
| Outcome set | `succeeded`, `failed`, `timed_out`, `cancelled`, `planned` | `completed`, `failed`, `interrupted`, `closed`, plus timeouts as errors | The five above; `planned` becomes dry-run output, not an outcome. |
| Cleanup report | Settlement reporting; limits documented | `cleanup` field, descendant identity tracking, repeated signalling | Counterpart's model, built: process group and session sweeps, descendant tracking by start time, repeated signalling, and an unconfirmed verdict that names pids, groups and the reason. |
| Permissions, stated up front | Provider options passed through, allowlisted | Tool lists, exact Bash guard hook, sandbox mode, deny rules | Provider-neutral modes over counterpart's enforcement. |
| Permissions, asked live | `respond`, interaction references, `Reject` mode | none | Later: take due's, behind `on_request: ask`. |
| Long-lived provider session | Resident engine | One process per run, resumed by session id | One process per run, continued by provider session id (`session.resume`, `follow_up`); a resident session is deferred (issue 1). |
| Queue and limits | Admission limits, 16 sessions | Queue, pool of 4 running, lifetime cap of 64 | Queue with admission limits (16 live workers, 4 running runs, 16 queued per worker); no lifetime cap. |
| MCP tools | `spawn`, `follow_up`, `inspect`, `wait`, `interrupt`, `respond`, `release_run`, `release_session`, `read_items` | `spawn_agent`, `send_message`, `followup_task`, `wait_agent`, `read_agent`, `list_agents`, `interrupt_agent`, `close_agent` | The seven above, then `respond`. |
| Compact and full responses | none | `detail: summary\|full` with a field audit | Take counterpart's idea, simpler. |
| Trace files and summaries | `--trace`, `--trace-file`, `due trace summarize` | Activity history in `read_agent` | Start with stderr progress and `read`. |
| Install into hosts | `due init` | `counterpart init` | One `delega plugin install` and one `delega plugin uninstall`, through the hosts' commands. |
| Onboard skills | `due skills list\|show` over skills embedded in the binary | One catalog served as `skill://counterpart/<name>/SKILL.md` and installed as a plugin; digests in the identity | One embedded catalog behind the CLI, the MCP Skills extension and the plugin. |
| Usage and cost | In the result | In the result, with scope notes | Take both. |
| Provider executable identity | `--binary`, provenance in the result | Identity resource | Provenance in the result and `doctor`. |

## Decisions

Settled by Josh on 2026-11-18.

| Question | Decision |
| --- | --- |
| Live permission asking in the first version | No. The first version only denies; asking comes later. |
| Default `timeout_secs` | 600 (10 minutes), stated in the result. |
| A run denied a permission that still finished | `completed`, with the denial listed. `refused` only when the run stopped because of it. |
| Names | `worker` and `run`; tools `start`, `follow_up`, `wait`, `read`, `list`, `interrupt`, `close`, and later `respond`. |
| Permission modes | `read`, `write`, `full`. |
| CLI scope | Foreground `run` only. |
| Tracing | Stderr progress only in the first version. |
| Exit status | 0 `completed`, 1 `failed`, 2 `refused`, 124 `timed_out`, 130 `interrupted`. |
| Onboard skills | Yes: one embedded catalog, reached by `delega skill`, by MCP resources through the Skills extension, and by the installed plugin. |

Settled by Josh on 2026-11-19 (issue 25, built under issue 84):

| Question | Decision |
| --- | --- |
| Where configuration lives | One optional user-level file, `~/.config/delega/config.toml` (under `XDG_CONFIG_HOME` when set), read by `run`, `mcp` and `doctor`. No project-level file: a cloned repository cannot change what a worker may do; profiles cover per-project setups. |
| Precedence | Built-in defaults, then the user file, then a profile, then explicit flags or tool arguments. `DELEGA_*` variables override only executables and paths. |

## Deferred

Not in the first version, to be taken up once the core meets the six points in
the README:

- Live asking: `on_request: ask`, `ask_timeout`, the `respond` tool, the
  `waiting_for_answer` state and `permissions.asked`. Depends on a long-lived
  provider session (issue 1).
- Trace files, heartbeats and trace summaries.
- Listing or stopping, from the CLI, runs started by another process.
- The review layer (issue 5).

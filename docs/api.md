# The delega API

This page is the reference for what a caller sees: the request and result
contract, the command line, the configuration file, the MCP server and the
skills. The supervisor behind MCP is in [supervisor.md](supervisor.md) and each
provider's behaviour is in [providers.md](providers.md). Parts marked
**Later** are not built.

## The model

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

| Outcome | Meaning |
| --- | --- |
| `completed` | The provider finished the assignment and returned a final answer. It does not say the work is correct. |
| `failed` | The provider or delega hit an error. `error.kind` says which. |
| `timed_out` | The run's time limit passed. |
| `interrupted` | The caller stopped it, or the caller went away. |
| `refused` | The run did not go ahead: the request was invalid, a limit was reached, or a permission the run needed was denied and it could not continue. |

A denial listed in `permissions.denied` does not by itself decide the outcome.
The run is `refused` with kind `permission_denied` when delega judges that it
stopped because of the denial ([providers.md](providers.md#outcomes)).

Error kinds are listed in [providers.md](providers.md#error-kinds). The surfaces
add these:

| Kind | From |
| --- | --- |
| `name_in_use`, `limit_reached`, `queue_full`, `worker_closed`, `no_session`, `preflight_timeout` | The supervisor ([MCP](#mcp), library). |
| `cwd_required` | `delega mcp` `start` ([MCP](#mcp)). |
| `runtime_unavailable`, `signals_unavailable` | `delega run` ([Command line](#command-line)). |
| `config_unreadable`, `config_invalid` | `delega run` and `delega mcp` ([Configuration file](#configuration-file)). |

## The request

One versioned JSON object. The CLI builds it from flags, MCP from tool
arguments, and the library from a struct. A partial request saved as a file is a
**profile**; explicit values override it.

| Field | Type | Notes |
| --- | --- | --- |
| `schema_version` | integer | Must be 1. Required in a profile file; MCP and the library default it to 1. |
| `provider` | `claude` or `codex` | Required after merging. |
| `prompt` | string | The assignment, not blank, at most 4 MiB. Sent to the provider on stdin. |
| `cwd` | path | Working directory of the worker. `delega run` and the library default to the caller's directory and resolve a relative path against it. MCP `start` requires an absolute path (`cwd_required`). Must be a directory, or `cwd_unusable`. |
| `name` | string | Label for the worker, at most 64 characters. Under the supervisor it must be unique among live workers (`name_in_use`). `delega run` checks it and does not report it. |
| `instructions` | string | Extra system instructions, at most 256 KiB. `delega run` takes it from a profile only. |
| `model` | string | Provider model name or alias, at most 256 bytes, not starting with `-`. Absent keeps the provider default. |
| `effort` | `low`, `medium`, `high` | Absent keeps the provider default. |
| `timeout_secs` | integer | 1 to 86400. Default 600. Reported as `timing.timeout_secs`. |
| `permissions` | object | See [Permissions](#permissions). |
| `answer_schema` | JSON Schema | Ask for a structured answer and validate it. An object or a boolean, at most 256 KiB; one that does not compile is `invalid_answer_schema`. |
| `session` | object | `ephemeral: bool`, or `resume: <provider session id>` (at most 256 bytes). Not both. A resumed session the provider cannot find fails `session_not_found`; one that reports another session id fails `session_mismatch` if it would otherwise have completed ([providers.md](providers.md#outcomes)). |
| `native_args` | list of strings | Provider options passed through an allowlist, at most 64. A rejected one is `native_arg_rejected`. |

A request that breaks the contract is refused `invalid_request`, with every problem listed; MCP rejects some bad arguments earlier as tool errors ([MCP](#mcp)).
The bounds are in `crates/delega-core/src/contract/limits.rs`; `delega schema
request` carries the ranges and list counts but not string lengths.

### Permissions

| Field | Values | Meaning |
| --- | --- | --- |
| `permissions.mode` | `read` (default), `write`, `build`, `full` | `read`: read the working directory, run only the listed commands. `write`: also edit files in the working directory. `build`: also run any command inside the provider's OS sandbox, with writes to the working directory and temporary directories and no network ([Build](providers.md#build)). `full`: no scope and no sandbox from delega. |
| `permissions.commands` | exact command strings | Commands the worker may run in `read` and `write`, at most 256 of 4096 bytes each. Must be empty with `build` (`invalid_request`). Not used in `full`. |
| `permissions.directories` | paths | Extra readable directories, at most 64. Each must exist (`directory_unusable`). Relative to the caller's directory for `delega run`, to `cwd` for MCP `start`. |
| `permissions.on_request` | `deny` (default), `ask` | `ask` passes eligible Claude Code permission requests to the MCP or Rust supervisor caller. `delega run` and Codex refuse it with `permissions_not_supported`. |
| `permissions.ask_timeout` | 1 to 600 seconds, default 120 | Deadline for each asked request. The wait counts toward the run's time limit. |
| `permissions.strict` | `false` (default), `true` | `true` refuses, before start, a request the provider cannot enforce as stated (`permissions_not_enforceable`). `false` runs it and adds a `provider_notes` entry saying what is wider. `full` is never refused by this check. |

The provider mappings are in [providers.md](providers.md#permission-mapping)
and what each provider cannot enforce is in
[What cannot be enforced](providers.md#what-cannot-be-enforced). In short,
`strict` refuses every Codex `read`, `write` and `build` request, and a Claude
Code `build` request or `permissions.commands` without a command guard. The
refusal message reads `permissions.strict is set and <provider> cannot enforce
these permissions as stated: <part>; <part>`. A dry run gives the same refusal.

`--strict` can only turn the field on, so a profile's `strict: true` stays on
for `delega run`. MCP `start` can pass `permissions.strict: false`.

A request is checked before start and the first refusal found is reported. The
order is in `build_with` in `crates/delega-core/src/provider/plan.rs` and, under
the supervisor, in `crates/delega-core/src/supervisor/core/admit.rs`. After
these checks, reading the provider's version (and, for Codex, its MCP servers)
can end in `unsupported_provider_version` or `provider_preparation_failed`; a
dry run does neither.

## The result

One versioned JSON object, the same from every surface.

| Field | Notes |
| --- | --- |
| `schema_version` | |
| `run` | Run id. |
| `worker` | Worker id and `name`. Only under the supervisor, and absent for a refused `start`. |
| `provider` | Absent only on a refusal for a request that named no provider. |
| `outcome` | One of the five above. |
| `text` | The answer. For other outcomes, partial text or empty. |
| `structured`, `structured_valid` | The structured answer when `answer_schema` was given, and whether it satisfied the schema. |
| `error` | `kind`, `message`, and `provider_exit_code` when the provider exited with one. Present unless `completed`. |
| `cleanup` | `confirmed: true` when no process of the run was found left; otherwise `confirmed: false`, a `reason`, and `pids` and `process_groups` when known. Known gaps: `crates/delega-core/src/process/mod.rs`. |
| `permissions` | `mode` applied; `denied`, the denials delega recognised (for Codex, commands Codex marks declined), up to a cap; `asked`, the live requests with their redacted input summary, decision, reason and whether the response was written. |
| `provider_notes` | Where the provider did not do what the request asked, or did it differently. |
| `applied` | What delega applied, as names, counts, paths and digests. See [`applied`](#applied). |
| `session_id` | The provider's session id, for `resume`. |
| `usage` | This run's own tokens in, out, cached, reasoning, and `cost_usd`, each when known. See [Usage](#usage). |
| `session_usage` | The provider's totals for the session through this run, when reported. |
| `timing` | Queued, started and finished times, duration, and `timeout_secs`. |
| `provenance` | delega version, provider executable path and version. `build` is not filled yet. |

A caller that only needs to know what happened reads `outcome`, `text`, `error`
and `cleanup`.

### Usage

| Provider | `usage` | `session_usage` |
| --- | --- | --- |
| Claude Code | The run's own tokens. `cost_usd` when the run's own cost is known; a resumed run without it has no `cost_usd`. | The session's total cost and tokens. |
| Codex | The thread's totals for a run that did not resume. None for a resumed run. | The thread's totals. |

Both come from the provider's final event, so a run that ends before it has
neither. Subtracting two runs' `session_usage` gives one run's consumption only
if nothing else used the session in between. Compact reads leave out `usage`
and `session_usage`. How the Claude Code cost is chosen:
`crates/delega-core/src/provider/claude.rs` (`usages`).

### `applied`

`applied` records what delega applied to the run. `provider_notes` say the same
in words. Values are names, counts, paths or digests, never an environment
value or a deny rule's text. Keys are sorted. A dry run carries it as
`dry_run.applied`, and a compact MCP `read` includes it.

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

| Part | Content | Absent when |
| --- | --- | --- |
| `config` | `path` of the configuration file read and the `sha256` of its bytes. `delega mcp` reports the file as it was when the server started. | No file was read. |
| `sources` | For each request field that was set, the layer that set it: `default`, `config`, `profile`, `flag` (`delega run`) or `argument` (MCP). | The caller did not say (a library start without `ProviderOptions::origin`). |
| `env` | `passed`: variables the worker received, by reason. `withheld`: the caller's variables not passed, split into those that can never be passed (calling-session variables and `DELEGA_*`) and the rest. `named`: names given with `--env` or the file's `[env]`. | The run was refused before it was prepared. |
| `deny_rules` | Settings files read with rule counts (`files`), files ignored with the reason (`skipped`), and rules passed (`applied`). | Codex, or a refusal before the settings files were read. |
| `context` | `settings`: `deny_rules` (only the deny rules of Claude Code's settings reach the worker). `instructions`: `false`, because delega stops Claude Code from loading `CLAUDE.md` files and auto memory on its own. | Codex, or a refusal before it was decided. |

`sources` keys are request field names, dotted for nested ones (such as
`permissions.mode`); `crates/delega-core/src/profile.rs` records them.
`timeout_secs`, `permissions.mode`, `permissions.on_request`,
`permissions.strict` and `session.ephemeral` are present as `default` unless a
layer set them; the others appear only when set. An explicit `ephemeral`
removes a lower layer's `resume`, and the reverse. A follow-up reports its
worker's `start` sources, with `prompt` (and `timeout_secs` when given) as
`argument`.

A refusal before preparation (an invalid request, an unusable directory, a
missing executable, unusable settings, `strict`, a bad `--env` name, or a
supervisor refusal) carries `config` and `sources` only. A refusal for the
configuration file itself has no `applied`. Which endings carry which parts:
`crates/delega-core/src/contract/applied.rs`.

## Command line

The CLI runs one assignment in the foreground and exits.

```sh
delega run --provider codex -p "Summarize this repository"
delega run --provider claude --stdin --output json < task.txt > result.json
delega run --request profile.json --stdin --timeout 20m --write < task.txt
delega run --provider codex --resume SESSION_ID -p "Now add tests"
```

| Command | Purpose |
| --- | --- |
| `delega run` | Run one assignment and print the answer, or the result as JSON. |
| `delega mcp` | Serve the MCP tools over stdio. Takes `-e/--env` as `run` does. See [MCP](#mcp). |
| `delega plugin install`, `delega plugin uninstall` | Install or remove the plugin and skills in Claude Code, Codex or both. See [Install and uninstall](#install-and-uninstall). |
| `delega schema request\|result` | Print the contracts built into this executable. |
| `delega skill list\|show NAME` | List the built-in skills, or print one. `show --output json` adds `content`. An unknown name lists the known ones on standard error and exits 2. See [Skills](#skills). |
| `delega doctor` | Report what a run would meet. See [Doctor](#doctor). |
| `delega version` | Print the version; `--output json` prints the build identity, the same document as `delega://identity`. |

`delega run` options:

| Option | Effect |
| --- | --- |
| `--request FILE` | Profile to start from (a regular file, at most 5.25 MiB). |
| `--provider`, `--name`, `--model`, `--effort` | The request fields. |
| `-p/--prompt TEXT`, `--stdin` | The prompt, from the argument or from standard input (UTF-8). Not both. A prompt starting with `-` is given as `--prompt=-text`. |
| `-C/--cwd DIR` | Working directory. |
| `--timeout` | `90s`, `20m`, `2h` or seconds. Outside 1 s to 24 h is refused `invalid_request`. |
| `--read`, `--write`, `--build`, `--full` | `permissions.mode`. One at most. |
| `--strict` | `permissions.strict: true`. |
| `--allow-command CMD`, `--add-dir DIR` | Repeatable. `permissions.commands` and `permissions.directories`. |
| `-e/--env NAME[=VALUE]` | Repeatable. Pass the caller's `NAME`, or set it to `VALUE`. |
| `--schema FILE` | JSON Schema for `answer_schema` (a regular file, at most 1 MiB). |
| `--resume ID`, `--ephemeral` | `session`. Not both. |
| `--output text\|json` | Answer text (default) or the result as JSON. |
| `--dry-run` | Show what would run, start nothing. |
| `--progress` | Progress lines on standard error. |
| `-- ARGS` | `native_args`. |

Behaviour:

- **Building the request.** The [configuration file](#configuration-file) is
  read first, then the profile, then each flag given replaces its field. Lists
  replace, not add. Relative paths (`--request`, `--schema`, `-C`, `--add-dir`,
  and `cwd` and `permissions.directories` in a profile) resolve against the
  caller's directory. `instructions` has no flag.
- **Claude Code settings.** A Claude worker inherits no hooks, `env`, plugins,
  skills, MCP servers, instructions, `CLAUDE.md` files or auto memory from the
  user's or project's settings. It gets their `permissions.deny` rules, which
  only narrow the run. An unusable settings file refuses the run
  (`settings_unreadable`, `deny_rules_over_limit`). Which files are read and
  skipped: [Deny rules](providers.md#deny-rules).
- **Environment.** A worker receives a fixed allowlist, not the caller's
  environment ([Environment policy](providers.md#environment-policy)).
  `--env NAME` adds the caller's value (ignored when unset); `--env NAME=VALUE`
  sets one (empty and `=` allowed). Prefer `NAME` for secrets: a value on the
  command line is visible in the process table. An invalid name is
  `invalid_env_name`; a calling-session variable, a `DELEGA_*` variable or a
  name delega sets for the provider is `forbidden_env_name`. The message gives
  the position, not the name. A secret-looking name is allowed and named in
  `provider_notes`, and a run that reaches environment resolution carries one
  note with the counts withheld and passed. A profile cannot carry `env`. The configuration file's `[env]`
  applies first, so a flag that sets the same variable wins.
- **Output.** `--output text` prints the answer to standard output only for
  `completed`, with a trailing newline added. A run that did not complete
  writes `delega run: <outcome>: <error.kind>: <error.message>` to standard
  error. Each denial is written as `delega run: denied: <request>` and
  unconfirmed cleanup as `delega run: cleanup not confirmed: ...`.
  `--output json` prints the whole result, whatever the exit status.
- **Exit status.**

  | Status | When |
  | --- | --- |
  | 0 | `completed` (or a dry run that would go ahead). |
  | 1 | `failed`, or a completed run whose result could not be written. |
  | 2 | `refused`, including usage errors. |
  | 124 | `timed_out`. |
  | 130 | `interrupted`, or a completed run whose result write was cut short by a signal. |

  A closed pipe on standard output (`| head -1`) does not change the status.
- **Refusals.** Problems in the request (an unusable configuration file, an
  unreadable profile or schema, a missing prompt or provider, `--prompt` with
  `--stdin`, a bound) are printed as a `refused` result, exit 2. In JSON,
  `error.message` lists each problem as `field: problem` separated by `; `; in
  text, each is a line `delega run: refused: <field>: <problem>`. A
  configuration file problem is reported before anything else. Such a result
  has no `worker` and no `started_at`. Only what the
  argument parser rejects (an unknown flag, two modes, `--resume` with
  `--ephemeral`, a malformed `--timeout`) is a plain usage error with no
  result. A failure to start the async runtime or signal handlers is a refused
  result (`runtime_unavailable`, `signals_unavailable`).
- **Interrupt.** SIGINT, SIGTERM, SIGHUP and SIGQUIT stop the worker and its
  descendants, then print the result with its cleanup verdict. Cleanup waits at
  most 10 s by default. Further signals are acknowledged once and ignored.
  SIGKILL cannot be handled: the worker keeps running without its time limit,
  so stop `delega` with SIGTERM or SIGINT.
- **Slow readers.** Writing the result waits for whoever reads standard output.
  A signal during that write stops it; after a signal already used to stop the
  worker, the write gets 3 s. A cut-short write exits with the outcome's status,
  or 130 for a completed run. Standard error never holds up the run: lines that
  cannot be written in time are dropped and counted in a last line,
  `progress: N events or output chunks were dropped: ...`. How the write and
  the status work: `crates/delega/src/commands/run/execute.rs`,
  `crates/delega/src/commands/output.rs` and
  `crates/delega/src/commands/run/log.rs`.
- **Dry run.** `--dry-run` validates the merged request and prints what would
  run without starting anything. JSON is `{"schema_version": 1, "dry_run":
  {...}}` with provider, program, args, `env_names`, `env`, cwd,
  `time_limit_secs`, `stdin_bytes`, the permission `mapping`, `notes`,
  `before_start`, `deny_rules` and `applied`. `env` groups passed names by
  reason (`base`, `network`, `provider`, `named`, `set_by_caller`,
  `set_by_delega`), lists `named_not_set` and gives `withheld_count`. For
  Claude, `deny_rules` lists files read, `skipped` files with the reason, and
  the user's rules under `user_deny_rules` (bounded, with `omitted`). No values,
  no withheld names, and the prompt, instructions and settings text are given
  as sizes. A request that would be refused prints the refusal and exits 2.
- **Progress.** `--progress` writes lines starting `progress: ` to standard
  error: `started <provider> (<executable>)`, one `<event type>: <summary>` per
  provider event, `denied: <request>` for each denial, and `finished:
  <outcome>` last. A summary gives a message's length, a tool or item name and
  status, or at most 80 characters of an error with credentials removed. A
  `denied:` line can name a command or a path. With `--dry-run` it writes
  nothing unless the request cannot be built. **Later:** trace files and
  heartbeats.
- **Provider executable.** `DELEGA_CLAUDE_BIN` and `DELEGA_CODEX_BIN`, then the
  configuration file, then `PATH`
  ([Finding the executable](providers.md#finding-the-executable)).
- **Test variables.** `DELEGA_TEST_NO_PROCESS_LISTING` and
  `DELEGA_TEST_SYSTEM_ROOT` are for the test harness and not part of the
  surface (`crates/delega-core/src/process/mod.rs`,
  `crates/delega/src/commands/mod.rs`).
- **Scope.** Foreground only. Supervising several workers is done over MCP.

### Doctor

`delega doctor` reports what a run in the current directory would meet, with
the [configuration file](#configuration-file) applied and no `--env`. It starts
no worker and writes nothing. It starts each provider's `--version` probe (5 s
limit) and one `ps` listing. It prints no environment value, deny rule or
file content.

For `claude` and `codex` each it reports:

- the executable and where it came from (`DELEGA_CLAUDE_BIN`,
  `DELEGA_CODEX_BIN`, `config`, `PATH`, or `option` for a library caller);
- the version, and for Claude Code whether the command hook interface is there
  (2.1.325 or later, needed by `--allow-command`);
- environment variable counts by reason, and how many would be withheld;
- for Claude Code, the deny rules: files read with counts, files skipped and
  why, rules applied;
- `managed_settings`, the administrator settings found for the provider. Each
  source has `path`, `kind`, `state` (`read`, `partly_read`, `unreadable`,
  `not_inspected`), `files` for a directory (`found`, `unreadable`,
  `limit_reached`), an optional `note`, `keys` (each a `name` and a
  `category`) and, for Codex, `other_keys` (a count). Keys are listed because
  they are present; no value is read. Locations, kinds, categories and what
  doctor cannot see: [Managed settings](providers.md#managed-settings).

It also reports the configuration file (the path, what chose it:
`DELEGA_CONFIG`, `XDG_CONFIG_HOME`, `HOME` or null; `found`; `read`; the dotted
names of the keys set; counts of `[env] pass` and `[env.set]`); the limits in
force for a run and for the supervisor, with the `[mcp]` section applied
([Limits and defaults](supervisor.md#limits-and-defaults)); the built-in skills
and their digests; and whether process inspection works.

**Problems.** Each has a `kind`, a message, the provider (null for the whole
system) and what it `blocks`: `run`, `command_grants`, `cleanup` or `nothing`.
A problem blocks `run` only where a run would be refused or fail for it.

| Kind | Blocks | Meaning |
| --- | --- | --- |
| `provider_not_startable` | `run` | The executable cannot be started (for example a missing interpreter). |
| `provider_not_found` | `run` | No executable found, or the override names none. |
| `version_probe_failed` | `command_grants` (Claude), `nothing` (Codex) | `--version` failed or was killed. Only a Claude run that grants commands is refused. |
| `version_probe_timed_out` | `command_grants` (Claude), `nothing` (Codex) | `--version` did not answer in 5 s. |
| `unsupported_provider_version` | `command_grants` | Claude Code older than 2.1.325, or unreadable version: `--allow-command` runs are refused. |
| `settings_unreadable` | `run` | A Claude Code settings file read for deny rules cannot be used. The message names it. |
| `deny_rules_over_limit` | `run` | The deny rules are over a bound. |
| `managed_mcp_exclusive` | `run` | A readable, parsing Claude Code `managed-mcp.json` conflicts with `--strict-mcp-config`; Claude runs are refused. Other managed settings are informational. |
| `config_unreadable`, `config_invalid` | `run` | The configuration file cannot be used. The rest of the report is given as if there were no file. |
| `invalid_env_name`, `forbidden_env_name` | `run` | A name in the file's `[env]` that `--env` would refuse. |
| `process_inspection_unavailable` | `cleanup` | Process listing failed, so no cleanup can be confirmed. Runs still start. |

Another refusal a run would give while being built is reported under its own
kind and blocks `run`. A provider is `ready` when nothing blocks `run` for it.

**Output.** Text by default, with control characters shown as `?`.
`--output json` prints one object (`schema_version` 1) with `ready`,
`version`, `cwd`, `providers`, `process_inspection`, `config`, `limits`,
`problems` and `skills`; schema `crates/delega-core/schemas/doctor.schema.json`.
`env` and `deny_rules` are null when the launch could not be built, and
`deny_rules` and `command_hook` are null for Codex.

**Exit status.**

| Status | When |
| --- | --- |
| 0 | At least one provider is ready, process inspection works and the configuration file, if any, can be used. |
| 1 | No provider is ready, the configuration file is refused, or process inspection is unavailable. |
| 2 | Doctor cannot run (for example an unusable current directory). |

A missing provider, or a problem that blocks only `command_grants`, does not
make the exit 1 while the other provider is ready.

### Install and uninstall

`delega plugin install` installs a plugin carrying the delega skills, with
`delega mcp` as its MCP server, into Claude Code, Codex or both. `delega plugin
uninstall` removes it. Both register and unregister only through the hosts' own
CLIs, and neither starts a server or calls a model.

```sh
delega plugin install --dry-run         # print the files and the host commands
delega plugin install                   # every host found
delega plugin install --host claude     # Claude Code only
delega plugin uninstall --host codex    # remove from Codex, keep the directory for Claude Code
delega plugin uninstall                 # remove from every recorded host, then the directory
```

Options for both: `--host claude|codex` (repeatable), `--install-dir DIR`,
`--dry-run` (prints the files and commands, changes nothing) and
`--output text|json`.

- **Managed directory.** `--install-dir DIR`, else `DELEGA_INSTALL_HOME`, else
  `$HOME/.local/share/delega`. Must be absolute. An existing directory is
  reused only if it is empty or holds a valid marker.
- **Hosts.** `DELEGA_CLAUDE_BIN` or `DELEGA_CODEX_BIN`, else the absolute
  entries of `PATH` (`find_host` in `crates/delega/src/commands/plugin/mod.rs`);
  the configuration file is not used. Every host named with `--host` must be
  found. Without `--host`, missing hosts are skipped and at least one must be
  found.
- **Files written** under the managed directory:

  | Path | Content |
  | --- | --- |
  | `plugin/delega/bin/delega` | A copy of the running executable. |
  | `plugin/delega/skills/<name>/SKILL.md` | One per built-in skill. |
  | `plugin/delega/IDENTITY.json` | The output of `delega version --output json`. |
  | `plugin/delega/.claude-plugin/plugin.json`, `plugin/delega/.mcp.json` | Claude Code manifests; the server is `${CLAUDE_PLUGIN_ROOT}/bin/delega mcp`. |
  | `plugin/delega/plugin.json`, `plugin/delega/mcp.json` | Codex manifests. |
  | `.claude-plugin/marketplace.json`, `.agents/plugins/marketplace.json` | The local marketplace `delega-local`. |
  | `.delega-install.json` | The ownership marker: the files written and the hosts registered. |

- **Plugin version.** The delega version plus `+` and the SHA-256 of the copied
  executable, so a changed executable refreshes the host's cache.

Host commands (`ID` is `delega@delega-local`, `DIR` the managed directory),
each limited to 300 s:

| Host | Install | Uninstall |
| --- | --- | --- |
| Claude Code | `claude plugin marketplace add DIR`, `claude plugin install ID`, `claude plugin update ID --scope user` | `claude plugin uninstall ID --scope user`, `claude plugin marketplace remove delega-local --scope user` |
| Codex | `codex plugin marketplace add DIR`, `codex plugin add ID` | `codex plugin remove ID`, `codex plugin marketplace remove delega-local` |

If a host command fails, that host's remaining commands are skipped, the other
host still runs, the files stay, and the status is 1; fix it and run again.

`delega plugin uninstall` checks each host's registrations first and stops,
keeping every file, when they do not point at the managed directory, the marker
is invalid, or a listing fails or exceeds 30 s
(`crates/delega/src/commands/plugin/uninstall.rs`). A host named with `--host` that the marker does not list is reported `not_registered`. With `--host` the directory is kept while
another host remains in the marker. It deletes only the files the marker lists.
How registrations are checked: `crates/delega/src/commands/plugin/registration.rs`;
what the marker may name: `crates/delega/src/commands/plugin/marker.rs`.

If the managed directory was deleted by hand, uninstall runs no host command.
Remove the registrations yourself, from a neutral directory: `claude plugin
uninstall delega@delega-local --scope user`, `claude plugin marketplace remove
delega-local --scope user`, `codex plugin remove delega@delega-local`, `codex
plugin marketplace remove delega-local`.

Do not run install or uninstall concurrently on one directory.

**After install.** Reload each host: in Claude Code start a new session or
reconnect `plugin:delega:delega` from `/mcp` (`/reload-plugins` does not
restart the server); in Codex start a new session. Then compare
`delega://identity` with `plugin/delega/IDENTITY.json`. Until then the old
server usually reports [`stale_server`](#stale-server).

The plugin's server starts with no `--env`; its workers get the fixed allowlist
plus the configuration file's `[env]`. The manifests set no `cwd`, which is why MCP `start` requires an
absolute `cwd`. Claude Code puts the plugin's `bin/` on the Bash tool's `PATH`.
Claude.ai and Cowork refuse plugins with a top-level `bin/`, so it cannot be
installed there.

`--output json` prints one document, also on failure: `operation` (`install` or
`uninstall`), `ok`, `dry_run`, `install_dir`, `hosts` (each with `status`,
`commands` and `reload`) and `error`. Schemas:
[`plugin-install.schema.json`](https://github.com/joshrotenberg/delega/blob/main/crates/delega/schemas/plugin-install.schema.json),
[`plugin-uninstall.schema.json`](https://github.com/joshrotenberg/delega/blob/main/crates/delega/schemas/plugin-uninstall.schema.json).

Exit status: 0 on success or dry run; 1 when a host command failed or something
was refused; 2 for a usage error (a relative directory, or no `HOME` without
`--install-dir` or `DELEGA_INSTALL_HOME`).

## Configuration file

One optional TOML file that `delega run`, `delega mcp` and `delega doctor`
start from. Schema: `crates/delega-core/schemas/config.schema.json`.

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
max_waits_in_flight = 32
```

**Location.** `DELEGA_CONFIG` when not empty (absolute; a missing file is
refused), else `$XDG_CONFIG_HOME/delega/config.toml` when `XDG_CONFIG_HOME` is
absolute, else `$HOME/.config/delega/config.toml` when `HOME` is absolute, else
none. No file in a repository or working
directory is read. A missing file at a default place means no configuration.
The file is read when a command starts; `delega mcp` reads it once.

**Keys.** `schema_version = 1` is required; everything else is optional, and an
unknown key is refused.

| Key | Value |
| --- | --- |
| `[defaults]` `provider`, `model`, `effort`, `timeout_secs` | As the [request](#the-request) fields, with the same bounds. `prompt` and `cwd` are refused. |
| `[defaults.permissions]` `mode`, `strict` | As `permissions.mode` and `permissions.strict`. |
| `[env]` `pass` | Names, as `--env NAME`. |
| `[env.set]` | Names and string values, as `--env NAME=VALUE`. Values are never printed. |
| `[providers.claude]`, `[providers.codex]` `executable` | Absolute path. `DELEGA_CLAUDE_BIN` or `DELEGA_CODEX_BIN` overrides it. |
| `[mcp]` | Supervisor limits, used by `delega mcp` and shown by `delega doctor`. The file can lower these, not raise them: `max_live_workers` 1 to 16, `max_running_runs` 1 to 4, `max_queued_per_worker` 0 to 16, `wait_ceiling_secs` 1 to 300, `max_waits_in_flight` 1 to 64. `wait_default_secs` (default 30) can be set from 0 up to the ceiling in force; a ceiling below 30 s set alone lowers the default to it. |

`[env]` names have the rules and refusals of `--env`. Error positions count
`pass` in file order and `set` keys alphabetically.

**Precedence.** Built-in defaults, then the file, then a profile, then flags or
tool arguments. Lists replace. `--strict` only turns strict on, so a file's
`strict = true` stays on for `delega run` unless a profile sets it false; MCP
can pass `permissions.strict: false`. `[env]` applies before `--env`. The only
variables that override the file are `DELEGA_CLAUDE_BIN`, `DELEGA_CODEX_BIN`
and `DELEGA_CONFIG`.

**Safety.** The file and its directory must be owned by the current user and
not writable by group or others, and the file must be a regular file of at most
1 MiB. A symbolic link is followed only when the user owns it and its
directory. How the check works: `crates/delega-core/src/config/file.rs`.

**Refusals.** A file that cannot be used is never ignored. The message names
the path and the key or line, never a value.

| Kind | When |
| --- | --- |
| `config_unreadable` | It cannot be used as a file: unreadable, not a regular file, wrong owner, group or other writable, an untrusted link, over 1 MiB, or `DELEGA_CONFIG` missing or relative. |
| `config_invalid` | Not UTF-8 or TOML (line and column given), an unknown key, a wrong type or out-of-bound value, a relative path, or `mcp.wait_default_secs` above the ceiling. At most ten problems listed. |
| `invalid_env_name`, `forbidden_env_name` | A name in `[env]` that `--env` would refuse. |

`delega run` prints it as a `refused` result (status 2), before any other
problem and also under `--dry-run`. `delega mcp` does not start: it writes
`delega mcp: refused: <kind>: <message>` and exits 2. `delega doctor` reports
it as a problem and exits 1.

**Where it applies.** `delega run`: defaults, environment, executables.
`delega mcp`: `[mcp]` limits, `[defaults]` under every `start`, and the
environment and executables of every worker. `delega doctor`: environment,
executables, limits, and a report of the file. Results report it as
`applied.config` and `applied.sources` ([`applied`](#applied)). A library
caller passes values in (`Limits::wait_default`, `Supervisor::start_with_origin`,
`ProviderOptions::origin`).

## MCP

The MCP server is for a caller that supervises several workers at once. The
supervisor behind it is described in [supervisor.md](supervisor.md).

| Tool | Arguments | Returns |
| --- | --- | --- |
| `start` | The request fields, or `profile` plus overrides. An absolute `cwd` is required. | After the request checks (at most 10 s): `run`, `worker` (absent when refused), `state` (`queued`, `running`, or `finished` for a refusal), `ahead` while queued, and `outcome` and `error` when refused. |
| `follow_up` | `worker`, `prompt`, optional `timeout_secs` | At once: the same fields as `start`, with `worker`. Queues behind a running run. |
| `wait` | `runs`, `workers` or both (at most 256 ids together), `timeout_secs`, `after` (cursor) | When a run finishes, an interaction needs an answer, or on timeout: `timed_out`, the `timeout_secs` applied, each named run and worker (with `changed` when `after` was given), `unknown` ids and a `cursor`. Also `released_by_shutdown: true` when shutdown released it, and `limit_reached: "max_waits_in_flight"` when too many waits were open. |
| `read` | `run` or `worker`, `detail`, `after`, `text_offset` | The result if finished; otherwise state, pending interactions and `progress`. `text_window` with `text_offset`, and a `cursor`. |
| `list` | none | Live workers, then closed ones still kept, each with state, name, provider, current run, queue length, last run and outcome, session id and `can_follow_up`; limits in force and in use (including `default_wait_secs`, `wait_ceiling_secs`, `max_waits_in_flight`, `waits_in_flight`); a `cursor`; and [`stale_server`](#stale-server) when it applies. |
| `interrupt` | `run` or `worker` | When the run stops, or after 30 s with `settled: false`: `settled`, `was_queued` and the compact result. |
| `respond` | `interaction`, `decision` (`allow`, `deny`), optional `note` (at most 256 bytes, discarded) | `accepted`, `unknown_interaction`, `already_answered`, `timed_out` or `closed`. |
| `close` | `worker` | Stops the running run (waits at most 30 s; `settled: false` if it has not stopped), discards queued ones, frees the slot: `already_closed`, `discarded`, `stopped_run`, `settled` and the stopped run's compact result. |

Rules:

- **`cwd`.** `start` needs an absolute `cwd` from the call or the profile,
  because the server's own directory is chosen by the host. Without one it is
  refused `cwd_required`. `follow_up` uses the worker's `cwd`. A relative
  `permissions.directories` entry resolves against `cwd`.
- **`start` arguments.** The request fields and `profile` (a path, resolved
  against the server's directory). Precedence is the configuration file's
  `[defaults]`, then `profile`, then the call. The input schema holds the
  ranges and list counts, so a value outside them is an invalid argument;
  string lengths are checked on the merged request (`invalid_request`, a
  refused run). There is no per-call environment argument. An error about a
  profile gives the path, the kind of error and the line and column, not the
  file's text.
- **Starting never waits for the run.** Only `wait`, `interrupt` and `close`
  wait, each bounded. Bounds are counted in awake time and also checked
  against wall time ([How the bounds count time](supervisor.md#how-the-bounds-count-time)).
- **Wait.** `timeout_secs` defaults to 30 and is capped at 300
  (`mcp.wait_default_secs`, `mcp.wait_ceiling_secs`); 0 returns at once. Keep
  waits short and pass the cursor back. At most 64 waits block at once
  (`mcp.max_waits_in_flight`); one beyond that returns at once with
  `timeout_secs: 0`, `timed_out: true` and `limit_reached`. A wait released by
  shutdown carries `released_by_shutdown: true`, and its runs need not have
  finished.
- **States.** A worker is `queued`, `running`, `idle`, `waiting_for_answer` or
  `closed`. A run is `queued`, `running`, or finished with an outcome
  ([States](supervisor.md#states)).
- **Detail.** `read` is compact by default and includes `applied`;
  `detail: "full"` adds every result field, the activity history and the
  supervisor's diagnostics for the run. `interrupt` and `close` return compact
  results.
- **Long answers.** A compact `read` returns the first 20000 characters with
  the full length; `text_offset` reads from a character offset. A structured
  answer over 16 KiB is left out of a compact read (`structured_omitted`,
  `structured_bytes`). Page by run id, because a worker's latest run can change
  between pages ([Reading](supervisor.md#reading)).
- **Limits.** 16 live workers, 4 running runs and 16 queued runs per worker by
  default; `[mcp]` can lower them. Beyond the worker limit `start` is refused
  `limit_reached`; beyond the queue limit `follow_up` is refused `queue_full`;
  beyond the running limit a run stays `queued`. Request checks share a 10 s
  deadline (`preflight_timeout`). See
  [Limits and defaults](supervisor.md#limits-and-defaults).
- **Refusals are runs.** A refused `start` or `follow_up` returns a run id whose
  result is `refused` (`invalid_request`, `name_in_use`, `limit_reached`,
  `cwd_required`, a provider refusal, `preflight_timeout`; for `follow_up`,
  `worker_closed`, `queue_full`, `no_session`). A refused `start` creates no
  worker.
- **Tool errors.** An unknown argument, an id that names nothing (except in
  `wait`, which lists it in `unknown`), a `start` with no `provider` or
  `prompt`, an unreadable profile, `read` or `interrupt` with neither or both
  of `run` and `worker`, a `wait` with no ids or over 256, and `start` or
  `follow_up` after shutdown began.
- **Retention.** Finished results stay readable for 256 results, 64 MiB or 1
  hour ([Retention](supervisor.md#retention)). Runs discarded by `close` or
  shutdown finish `interrupted`.
- **Follow-ups** reuse the worker's first request and its session
  ([Follow-ups](supervisor.md#follow-ups)).
- **Ownership.** Workers belong to the connection that started them and stop
  when it closes.
- **Results.** Every tool returns structured content and the same JSON as text.
  Tool schemas are under `crates/delega-mcp/schemas/mcp/`.
- **History is data.** Text a worker returns is never an instruction to the
  caller.

### Stale server

A running host session keeps the server it started with. After `delega plugin
install` or `uninstall` replaces or removes that executable, `list` and
`delega://identity` carry a `stale_server` object until the server is
reconnected or a new session starts. A call whose check times out (2 s) or
finds too many checks pending omits it.

| Field | Meaning |
| --- | --- |
| `change` | `replaced` or `removed`. |
| `running_revision` | The revision of the running build. |
| `installed_revision` | The revision in the installed `IDENTITY.json`, when it can be read. |
| `what_to_do` | Reconnect the server (for example from `/mcp` in Claude Code) or start a new session. |

A changed file is not proof of a newer build, so compare the two revisions. How
the check works: `crates/delega-mcp/src/stale.rs`.

### Running the server

`delega mcp` serves one connection on standard input and output. Standard
output carries protocol messages only; diagnostics go to standard error,
starting with `delega mcp: <version> ready`. At most 128 calls are handled at
once, and one input message may be at most 32 MiB.

- **Configuration.** The [configuration file](#configuration-file) is read at
  start; restart the server to pick up a change. `--env` and
  `DELEGA_CLAUDE_BIN`/`DELEGA_CODEX_BIN` apply as for `run`. A bad `--env` or
  file stops start-up with `delega mcp: refused: <kind>: <message>` and
  status 2.
- **Stopping.** End of input or SIGINT, SIGTERM, SIGHUP, SIGQUIT stops new
  calls, releases open waits, and stops the workers, waiting at most 30 s (runs
  still unsettled then give exit status 1), then waits
  at most 15 s for the transport and exits. A second signal is acknowledged and
  ignored. SIGKILL leaves the workers running. A client that stops reading
  standard output should signal the server.
- **Exit status.**

  | Status | When |
  | --- | --- |
  | 0 | End of input. |
  | 130 | A signal. |
  | 1 | A run's cleanup was not confirmed, the supervisor failed, or the transport failed after end of input. Wins over 0 and 130. |
  | 2 | The server could not start. |

How stopping works: `crates/delega-mcp/src/serve.rs` and
`crates/delega/src/commands/mcp.rs`.

## Skills

Skills are short documents that tell a calling agent how to use delega. They
are built into the executable, so they match the installed behaviour.

| Door | How |
| --- | --- |
| CLI | `delega skill list`, `delega skill show NAME`; `--output json` for a versioned catalog. |
| MCP | Listed by `skills/list` and `resources/list`, read at `skill://delega/<name>/SKILL.md`. |
| Plugin | Installed by [`delega plugin install`](#install-and-uninstall). |

All three serve the same bytes. The build identity (`delega version --output
json`, and `delega://identity` over MCP) has `name`, `version`,
`schema_version`, `revision` (12 characters or `unknown`), `features` (empty)
and `skills` (name, URI and SHA-256 of each). Over MCP it can also carry
[`stale_server`](#stale-server). Reading a skill grants nothing.

| Skill | For |
| --- | --- |
| `run` | Calling `delega run` from a script or another agent. |
| `delegate` | Supervising workers over MCP. |

Skills live under `crates/delega-core/skills/<name>/SKILL.md`. A test fails if
a skill names a command, flag, tool or argument the build does not offer.

## Library

The CLI and MCP server are layers over one Rust API with the same nouns: build a
`Request`, `start` it on a `Supervisor`, then `wait`, `read`, `follow_up`,
`interrupt`, `respond`, `close`, `list` and `shutdown`. `delega run` uses the
provider layer's `run` directly.

A program that embeds the library and spawns its own processes on other threads
should hold `delega_core::process::spawn_guard()` across each spawn call, and
only there (not across an `.await` or a call into delega). Without it, on
macOS a child can inherit a worker's pipes.

What is not built yet is listed in [Status and limits](project/status.md#scope).

# The delega API: a proposal

Status: agreed design. The request and result contract, the CLI commands
`run`, `schema`, `skill` and `version`, and the provider layer behind `run` are
built; `mcp`, `init`, `doctor` and the library `Engine` are not. Josh settled
the open questions on 2026-11-18; the decisions and what was deferred are listed
at the end. Parts marked **Later** are not in the first version.

The aim is to fix the surface first, then work backwards from it to the engine
and forwards from the parts [due](https://github.com/joshrotenberg/due) and
[counterpart](https://github.com/joshrotenberg/counterpart) already have. The
last section maps every part of this surface to where it would come from.

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
`completed`, with the denial listed in `permissions.denied`. It is `refused`
only when the run stopped because of the denial.

## The request

One JSON object, versioned. The CLI builds it from flags, MCP from tool
arguments, and the library from a struct. A partial request saved as a file is a
**profile**; explicit values override it.

| Field | Type | Notes |
| --- | --- | --- |
| `schema_version` | integer | Required in a file. |
| `provider` | `claude` or `codex` | Required after merging profile and flags. |
| `prompt` | string | The assignment. Always sent to the provider on stdin. |
| `cwd` | path | Working directory of the worker. Must exist. |
| `name` | string | Optional label for the worker, unique among live workers. |
| `instructions` | string | Extra system instructions for the worker. |
| `model` | string | Provider model name or alias. Absent keeps the provider default. |
| `effort` | `low`, `medium`, `high` | Absent keeps the provider default. |
| `timeout_secs` | integer | Limit for the run in whole seconds, 1 to 86400. Default 600 (10 minutes). The limit applied is stated in the result as `timing.timeout_secs`. |
| `permissions` | object | See below. |
| `answer_schema` | JSON Schema | Ask for a structured answer and validate it. |
| `session` | object | `ephemeral: bool`, or `resume: <provider session id>` to continue saved history in a new worker. |
| `native_args` | list of strings | Provider options passed through, restricted by an allowlist. The escape hatch. |

### Permissions

What a worker may do is part of the request, in provider-neutral terms, and the
result reports what happened.

| Field | Values | Meaning |
| --- | --- | --- |
| `permissions.mode` | `read` (default), `write`, `full` | `read`: may read the working directory, no writes, no commands except those listed in `permissions.commands`. `write`: may also edit files in the working directory; commands are still only those listed. `full`: no restriction from delega. |
| `permissions.commands` | list of exact command strings | Shell commands the worker may run, matched exactly. They add to `read` and `write`; `full` already allows every command. |
| `permissions.directories` | list of paths | Extra directories the worker may read. |
| `permissions.on_request` | `deny` | What happens when the provider asks for something not granted: answered no at once and recorded in the result. **Later:** `ask`, which surfaces the request to the caller as a pending interaction, with `permissions.ask_timeout` for how long it waits before it is answered no. |

Each mode maps to concrete provider settings (Claude tool lists, hooks and
permission mode; Codex sandbox and approval policy). The mapping is documented
per provider, and where a provider cannot enforce part of it the result says so
in `provider_notes`.

The first version only denies. Asking needs a long-lived provider session,
which is due's resident engine and the subject of issue 1; `deny` works with
one process per run.

## The result

One JSON object, versioned, the same from every surface.

| Field | Notes |
| --- | --- |
| `schema_version` | |
| `run` | Run id. |
| `worker` | Worker id and `name`, if any. |
| `provider` | |
| `outcome` | One of the five above. |
| `text` | The final answer, or empty. |
| `structured` | The structured answer when `answer_schema` was given, with `structured_valid: bool`. |
| `error` | `kind`, `message`, `provider_exit_code`. Present unless `completed`. |
| `cleanup` | `confirmed: bool`, and when false, what could not be confirmed stopped (pids, process groups) and why. |
| `permissions` | `mode` applied, and `denied`: requests answered no. **Later:** `asked`, requests the caller answered. |
| `provider_notes` | Where this provider did not do what the request asked, or did it differently. |
| `session_id` | The provider's own session id, for the provider's `resume`. |
| `usage` | Tokens in and out, cached, reasoning; `cost_usd` when the provider reports it. |
| `timing` | Queued, started and finished times, duration, and the time limit applied as `timeout_secs`. |
| `provenance` | delega version and build, provider executable path and version. |

A caller that only needs to know what happened reads `outcome`, `text`, `error`
and `cleanup`.

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
| `delega mcp` | Serve the MCP tools over stdio. |
| `delega init` | Install the plugin and skill into Claude Code, Codex or both, through the hosts' own commands. `--dry-run` shows what it would do. |
| `delega schema request\|result` | Print the contracts built into this executable. |
| `delega skill list\|show NAME` | List the skills built into this executable, or print one. See [Skills](#skills). |
| `delega doctor` | Report providers found, their versions, limits in force and anything that would stop a run. |
| `delega version` | Version, source revision and features; `--output json`. |

`delega run` options: `--request FILE`, `--provider`, `-p/--prompt`, `--stdin`,
`-C/--cwd`, `--name`, `--model`, `--effort`, `--timeout`, `--read`, `--write`,
`--full`, `--allow-command CMD` (repeatable), `--add-dir DIR` (repeatable),
`-e/--env NAME[=VALUE]` (repeatable), `--schema FILE`, `--resume ID`, `--ephemeral`,
`--output text|json`, `--dry-run`, and provider options after `--`.

Behavior:

- **Timeout.** `--timeout` accepts `90s`, `20m`, `2h` or a plain number of
  seconds, and the CLI converts it to the request's `timeout_secs`.
- **Environment.** A worker does not inherit the caller's environment. It
  receives a fixed base (the user, shell, locale, temporary and base directory
  variables), proxy and certificate variables, and the provider's own
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
  a value. Every result carries one note with the number of variables withheld
  and passed. `--env` is caller-side: a profile file does not carry it, and an
  `env` or `pass_env` field in one is refused as an unknown field.
- **Building the request.** The profile named by `--request` is read first and
  every flag that was given replaces the matching field; a flag that was not
  given leaves the profile's value. Lists (`--allow-command`, `--add-dir`,
  provider options after `--`) replace the profile's list, they are not added
  to it. `--read`, `--write` and `--full` replace `permissions.mode`;
  `--resume` replaces `session.ephemeral` and `--ephemeral` replaces
  `session.resume`. The prompt comes from `-p` or from `--stdin`, which reads
  standard input as UTF-8 up to the prompt limit; giving both is a refusal, not
  a usage error. `--schema FILE` is read as JSON and becomes `answer_schema`.
  The profile and the schema file must be regular files; a device or a pipe is
  refused, and so is a file over the limit (the prompt, instructions and
  headroom for a profile; four times the schema limit for a schema), with the
  limit named. A prompt that starts with a hyphen is given as `--prompt=-text` (or `-p=-text`); `-p -text` is a usage error, so `-p` never swallows the flag after it. Every relative path
  resolves against the caller's working directory: `--request`, `--schema`,
  `-C`, `--add-dir`, and `cwd` and `permissions.directories` in a profile. (The
  provider layer would otherwise resolve a relative extra directory against the
  worker's own working directory, so the CLI makes the paths absolute first.)
- **Output.** `--output text` (the default) prints the answer text to standard
  output, with a newline added when it lacks one, and nothing else there, and
  only when the outcome is `completed`. For any other outcome nothing is printed
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
- **Refusal before a run exists.** An unreadable or invalid profile, a missing
  prompt or provider, `--prompt` together with `--stdin`, an unreadable or
  malformed schema file, and any other validation problem are printed as a
  `refused` result, not as a usage error: with `--output json` the result has
  `error.kind` `invalid_request` and the message lists every problem found, as
  `field: problem` separated by `; `; with text output each problem is a line
  on standard error, `delega run: refused: <field>: <problem>`, and standard
  output stays empty. The exit status is 2 in both. Only an argument clap
  rejects (an unknown flag, `--write` with `--full`, `--resume` with
  `--ephemeral`, a malformed `--timeout`) is a plain usage error with its own
  message and no result. Such a result has no `worker`, no `started_at` and a
  confirmed `cleanup`, and the CLI makes its run id.
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
  deadline, after which the result says what could not be confirmed. A signal
  during `--dry-run`, or before the handlers are installed at startup, has no
  worker to stop and takes the default action. **SIGKILL cannot be handled**:
  if `delega` is killed with it, or crashes without running its exit code, the
  worker keeps running and nothing enforces its time limit. Stop `delega` with
  SIGTERM or SIGINT, not SIGKILL.
- **Writing the result waits on the reader.** After the worker is stopped,
  `delega` writes the result to standard output on a thread of its own, and that
  write waits for whoever reads standard output; a large result and a reader that
  does not read can hold it indefinitely. The signal handlers stay installed
  during the write, and a SIGINT, SIGTERM, SIGHUP or SIGQUIT ends `delega` at
  once, with the outcome's exit status, or 130 when the outcome was `completed`
  because its result was not delivered. The worker is already gone by then.
  If a signal had already been received during the run (it was used to stop
  the worker), the write is given 3 seconds and `delega` then exits the same
  way.
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
  cwd, `time_limit_secs`, `stdin_bytes`, the permission `mapping`, `notes` and
  `before_start`. `env` groups the names that would be passed by reason
  (`base`, `network`, `provider`, `named`, `set_by_caller`, `set_by_delega`),
  lists names given as `--env NAME` that are not set (`named_not_set`) and gives only the count of
  the caller's variables that are withheld (`withheld_count`); text output
  prints the same groups. It lists environment variable names, never values,
  never the names of withheld variables, and not the assignment, instructions or settings text, which are replaced by their
  size. A request that would be refused prints the refusal as for a run (a
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
  a path. `finished` is always the last line, also for a request refused before
  it starts, including one refused while it is built. **Later:** due's trace files, heartbeats and
  summaries.
- **Provider executable.** The CLI honours `DELEGA_CLAUDE_BIN` and
  `DELEGA_CODEX_BIN` in its own environment, as the provider layer does
  ([providers.md](providers.md#finding-the-executable)). There is no flag for
  it. `--allow-command` for Claude Code is enforced by this executable's own
  hidden `command-guard` subcommand, which Claude Code starts before a `Bash`
  call; it is not part of the public surface.
- **Test-only variable.** `DELEGA_TEST_NO_PROCESS_LISTING`, when set to any value, makes process inspection unavailable so `cleanup` is reported as not confirmed; it is for the acceptance harness, can only make the verdict weaker, and is not part of the public surface.
- **Scope.** The CLI runs in the foreground only. It does not list or stop runs
  started by another process; supervising several workers is done over MCP.

## MCP

The MCP server is for a caller that supervises several workers at once: start
them, keep working, collect results. Seven tools in the first version, and an
eighth, `respond`, when live asking is added.

| Tool | Arguments | Returns |
| --- | --- | --- |
| `start` | The request fields, or `profile` plus overrides | At once: worker id, run id, state `queued` or `running`. |
| `follow_up` | `worker`, `prompt`, optional `timeout_secs` | At once: the new run id. Queues behind a running run. |
| `wait` | `runs` or `workers` (list), `timeout_secs`, `after` (cursor) | When any of them finishes or needs an answer, or on timeout: their current states and a cursor. Never an error on timeout. |
| `read` | `run` or `worker`, `detail`, `after` (cursor) | The result if finished; otherwise state, partial text and recent activity. |
| `list` | none | Workers on this connection, their state, queue length and session ids. |
| `interrupt` | `run` or `worker` | After the run has stopped and cleanup has been attempted: the result, with `interrupted`. |
| `respond` (**Later**) | `interaction`, `decision` (`allow`, `deny`), optional `note` | Acknowledgement. Only with `on_request: ask`. |
| `close` | `worker` | Stops the worker, discards queued assignments, frees its slot. Results already produced stay readable for a bounded time. |

Rules that hold for every tool:

- **Starting returns at once.** Only `wait` and `interrupt` wait for anything,
  and both are bounded.
- **States** a worker can be in: `queued`, `running`, `idle`, `closed`, and
  **later** `waiting_for_answer`. A run is `queued`, `running`, or finished with
  an outcome.
- **Detail.** Every response is compact by default. `detail: "full"` adds
  process diagnostics, activity history and every field of the result.
- **Limits** are admission limits on live workers and running runs, reported by
  `list` and `delega doctor`. A closed worker frees its slot. Reaching a limit
  is a `refused` outcome with the limit named, not a dead end that needs a
  restart.
- **Ownership.** Workers belong to the connection that started them and stop
  when it closes.
- **History is data.** Text a worker returns is never an instruction to the
  caller.

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
  holding a `SKILL.md` with name and description frontmatter and any supporting
  files. The build embeds them as one catalog. Nothing is read from disk at run
  time.
- **CLI.** `delega skill list` prints names and descriptions, and
  `delega skill show NAME` prints one. `--output json` gives a versioned
  catalog a program can select from.
- **MCP.** The server offers the same catalog through the MCP Skills extension
  (SEP-2640): each skill is discoverable and readable as a resource at
  `skill://delega/<name>/SKILL.md`, with its supporting files beside it. A host
  without the extension can still read them as ordinary resources.
- **Plugin.** `delega init` installs the same files as a plugin into Claude Code
  and Codex, for hosts that load skills from disk.

Rules:

- **Same bytes everywhere.** The three doors serve identical content. The build
  identity (`delega version`, and the server's identity over MCP) carries a
  digest of each skill, so a caller can tell which guidance it has.
- **Skills only describe what exists.** A test fails if a skill names a command,
  flag, tool or argument that this build does not offer. A build without the MCP
  server does not ship the skill that describes it.
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
nouns: build a `Request`, `start` it on an `Engine` to get a `Worker` and a
`Run`, `wait` for the `Result`, `follow_up`, `interrupt`, `close`. Anything both
surfaces compute lives in the library.

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

| Part of this surface | due | counterpart | Proposal |
| --- | --- | --- | --- |
| `run` command, profiles, `--dry-run`, `--stdin` | `due run`, `--request`, `--dry-run` | none | Take due's. |
| Request and result as versioned JSON Schema | `run-request`, `run-result` schemas, `due schema` | Tool schemas in `contract/`, no versioning promise | Take due's shape; add `outcome`, `cleanup`, `permissions`, `provider_notes`. |
| Outcome set | `succeeded`, `failed`, `timed_out`, `cancelled`, `planned` | `completed`, `failed`, `interrupted`, `closed`, plus timeouts as errors | The five above; `planned` becomes dry-run output, not an outcome. |
| Cleanup report | Settlement reporting; limits documented | `cleanup` field, descendant identity tracking, repeated signalling | Take counterpart's, pending issue 1. |
| Permissions, stated up front | Provider options passed through, allowlisted | Tool lists, exact Bash guard hook, sandbox mode, deny rules | Provider-neutral modes over counterpart's enforcement. |
| Permissions, asked live | `respond`, interaction references, `Reject` mode | none | Later: take due's, behind `on_request: ask`. |
| Long-lived provider session | Resident engine | One process per run, resumed by session id | Decided by issue 1. |
| Queue and limits | Admission limits, 16 sessions | Queue, pool of 4 running, lifetime cap of 64 | Queue with admission limits; no lifetime cap. |
| MCP tools | `spawn`, `follow_up`, `inspect`, `wait`, `interrupt`, `respond`, `release_run`, `release_session`, `read_items` | `spawn_agent`, `send_message`, `followup_task`, `wait_agent`, `read_agent`, `list_agents`, `interrupt_agent`, `close_agent` | The seven above, then `respond`. |
| Compact and full responses | none | `detail: summary\|full` with a field audit | Take counterpart's idea, simpler. |
| Trace files and summaries | `--trace`, `--trace-file`, `due trace summarize` | Activity history in `read_agent` | Start with stderr progress and `read`. |
| Install into hosts | `due init` | `counterpart init` | One `init`, through the hosts' commands. |
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

## Deferred

Not in the first version, to be taken up once the core meets the six points in
the README:

- Live asking: `on_request: ask`, `ask_timeout`, the `respond` tool, the
  `waiting_for_answer` state and `permissions.asked`. Depends on a long-lived
  provider session (issue 1).
- Trace files, heartbeats and trace summaries.
- Listing or stopping, from the CLI, runs started by another process.
- The review layer (issue 5).

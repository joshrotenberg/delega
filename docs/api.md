# The delega API: a proposal

Status: agreed design, nothing here is implemented yet. Josh settled the open
questions on 2026-11-18; the decisions and what was deferred are listed at the
end. Parts marked **Later** are not in the first version.

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
| `timeout` | duration | Limit for the run. Default 10 minutes. The limit applied is stated in the result. |
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
| `timing` | Queued, started and finished times, duration, and the time limit applied. |
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
`--schema FILE`, `--resume ID`, `--ephemeral`, `--output text|json`,
`--dry-run`, and provider options after `--`.

Behavior:

- **Exit status** follows the outcome: 0 `completed`, 1 `failed`, 2 `refused`
  (including usage errors), 124 `timed_out`, 130 `interrupted`. With
  `--output json` the result is always printed, whatever the exit status.
- **Interrupt.** Ctrl-C or SIGTERM stops the run, cleans up, prints the result
  with `interrupted`, then exits.
- **Dry run.** `--dry-run` validates the merged request and prints the provider
  executable, arguments, working directory and applied permissions without
  starting anything.
- **Progress.** `--progress` writes one line per provider event to stderr.
  **Later:** due's trace files, heartbeats and summaries.
- **Scope.** The CLI runs in the foreground only. It does not list or stop runs
  started by another process; supervising several workers is done over MCP.

## MCP

The MCP server is for a caller that supervises several workers at once: start
them, keep working, collect results. Seven tools in the first version, and an
eighth, `respond`, when live asking is added.

| Tool | Arguments | Returns |
| --- | --- | --- |
| `start` | The request fields, or `profile` plus overrides | At once: worker id, run id, state `queued` or `running`. |
| `follow_up` | `worker`, `prompt`, optional `timeout` | At once: the new run id. Queues behind a running run. |
| `wait` | `runs` or `workers` (list), `timeout`, `after` (cursor) | When any of them finishes or needs an answer, or on timeout: their current states and a cursor. Never an error on timeout. |
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
| Default `timeout` | 10 minutes, stated in the result. |
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

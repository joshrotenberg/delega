# The delega API: a proposal

Status: proposal, nothing here is implemented. It exists to be argued with.
Open questions are collected at the end and marked **Open** where they arise.

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

**Open:** whether a run that finishes after being denied one permission is
`completed` with the denial listed, or `refused`. Proposal: `completed` if the
provider returned a final answer, with the denial in `permissions.denied`;
`refused` only when the run stopped because of it.

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
| `timeout` | duration | Limit for the run. **Open:** due has no default; counterpart defaults to 10 minutes. Proposal: a default, stated in the result. |
| `permissions` | object | See below. |
| `answer_schema` | JSON Schema | Ask for a structured answer and validate it. |
| `session` | object | `ephemeral: bool`, or `resume: <provider session id>` to continue saved history in a new worker. |
| `native_args` | list of strings | Provider options passed through, restricted by an allowlist. The escape hatch. |

### Permissions

What a worker may do is part of the request, in provider-neutral terms, and the
result reports what happened.

| Field | Values | Meaning |
| --- | --- | --- |
| `permissions.mode` | `read` (default), `write`, `full` | `read`: may read the working directory, no writes, no commands. `write`: may edit files in the working directory. `full`: no restriction from delega. |
| `permissions.commands` | list of exact command strings | Shell commands the worker may run, matched exactly. |
| `permissions.directories` | list of paths | Extra directories the worker may read. |
| `permissions.on_request` | `deny` (default), `ask` | What happens when the provider asks for something not granted. `deny`: answered no at once, recorded in the result. `ask`: surfaced to the caller as a pending interaction to answer. |
| `permissions.ask_timeout` | duration | With `ask`, how long a request waits before it is answered no. |

Each mode maps to concrete provider settings (Claude tool lists, hooks and
permission mode; Codex sandbox and approval policy). The mapping is documented
per provider, and where a provider cannot enforce part of it the result says so
in `provider_notes`.

**Open:** whether `ask` is in the first version. It needs a long-lived provider
session, which is due's resident engine and the subject of issue 1. `deny` works
with one process per run.

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
| `permissions` | `mode` applied, `denied`: requests answered no, `asked`: requests the caller answered. |
| `provider_notes` | Where this provider did not do what the request asked, or did it differently. |
| `session_id` | The provider's own session id, for the provider's `resume`. |
| `usage` | Tokens in and out, cached, reasoning; `cost_usd` when the provider reports it. |
| `timing` | Queued, started and finished times, and duration. |
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
  **Open:** due has a fuller trace (files, heartbeats, summaries); proposal is
  to start with stderr progress only.
- **Permissions.** The CLI is unattended, so `on_request` is always `deny`.

## MCP

The MCP server is for a caller that supervises several workers at once: start
them, keep working, collect results. Eight tools.

| Tool | Arguments | Returns |
| --- | --- | --- |
| `start` | The request fields, or `profile` plus overrides | At once: worker id, run id, state `queued` or `running`. |
| `follow_up` | `worker`, `prompt`, optional `timeout` | At once: the new run id. Queues behind a running run. |
| `wait` | `runs` or `workers` (list), `timeout`, `after` (cursor) | When any of them finishes or needs an answer, or on timeout: their current states and a cursor. Never an error on timeout. |
| `read` | `run` or `worker`, `detail`, `after` (cursor) | The result if finished; otherwise state, partial text and recent activity. |
| `list` | none | Workers on this connection, their state, queue length and session ids. |
| `interrupt` | `run` or `worker` | After the run has stopped and cleanup has been attempted: the result, with `interrupted`. |
| `respond` | `interaction`, `decision` (`allow`, `deny`), optional `note` | Acknowledgement. Only with `on_request: ask`. |
| `close` | `worker` | Stops the worker, discards queued assignments, frees its slot. Results already produced stay readable for a bounded time. |

Rules that hold for every tool:

- **Starting returns at once.** Only `wait` and `interrupt` wait for anything,
  and both are bounded.
- **States** a worker can be in: `queued`, `running`, `waiting_for_answer`,
  `idle`, `closed`. A run is `queued`, `running`, or finished with an outcome.
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

**Open:** counterpart has two ways to add work to a worker, `send_message`
(queue input for a running worker) and `followup_task` (another assignment in
the same conversation). Proposal: one tool, `follow_up`, which queues.

**Open:** due separates releasing a run from releasing a session. Proposal: one
`close`, with finished runs kept for a bounded time and count.

## Library

The CLI and the MCP server are thin layers over one Rust API with the same
nouns: build a `Request`, `start` it on an `Engine` to get a `Worker` and a
`Run`, `wait` for the `Result`, `follow_up`, `interrupt`, `close`. Anything both
surfaces compute lives in the library.

## Not in this surface

- **Review.** An enforced independent review of a worker's change (counterpart's
  pair) is a later layer with its own tools, built on these eight. What it needs
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
| Permissions, asked live | `respond`, interaction references, `Reject` mode | none | Take due's, behind `on_request: ask`. |
| Long-lived provider session | Resident engine | One process per run, resumed by session id | Decided by issue 1. |
| Queue and limits | Admission limits, 16 sessions | Queue, pool of 4 running, lifetime cap of 64 | Queue with admission limits; no lifetime cap. |
| MCP tools | `spawn`, `follow_up`, `inspect`, `wait`, `interrupt`, `respond`, `release_run`, `release_session`, `read_items` | `spawn_agent`, `send_message`, `followup_task`, `wait_agent`, `read_agent`, `list_agents`, `interrupt_agent`, `close_agent` | The eight above. |
| Compact and full responses | none | `detail: summary\|full` with a field audit | Take counterpart's idea, simpler. |
| Trace files and summaries | `--trace`, `--trace-file`, `due trace summarize` | Activity history in `read_agent` | Start with stderr progress and `read`. |
| Install into hosts | `due init` | `counterpart init` | One `init`, through the hosts' commands. |
| Usage and cost | In the result | In the result, with scope notes | Take both. |
| Provider executable identity | `--binary`, provenance in the result | Identity resource | Provenance in the result and `doctor`. |

## Open questions, collected

1. Is live asking (`on_request: ask`) in the first version, or only `deny`?
2. A default `timeout`, and its value.
3. `completed` with a denial listed, or `refused`, when a run was denied a
   permission but still finished.
4. The names: `worker` and `run`, and the tool names `start`, `follow_up`,
   `wait`, `read`, `list`, `interrupt`, `respond`, `close`.
5. Whether the three permission modes are the right provider-neutral set, and
   whether `full` should exist.
6. Whether the CLI needs anything beyond foreground `run`, such as listing or
   stopping runs started by another process.
7. How much of due's tracing to carry in the first version.
8. Exit status values.

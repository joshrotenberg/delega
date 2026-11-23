# The supervisor

`delega_core::supervisor` runs several workers at once. It is the library behind
the [MCP tools](api.md#mcp): each tool corresponds to one operation of
[`Supervisor`](https://github.com/joshrotenberg/delega/blob/main/crates/delega-core/src/supervisor/mod.rs).
This page covers what a library caller sees; [api.md](api.md#mcp) covers the
tools and where their arguments differ.

A worker is one provider conversation. A run is one assignment given to it.
`start` makes a worker and its first run; `follow_up` adds a run to the same
worker, which continues the worker's provider session.

## Operations

| Operation | MCP tool | Waits for a provider? | Returns |
| --- | --- | --- | --- |
| `start(Request)` | `start` | no | `StartResponse`: worker id, run id, state `queued` or `running`. |
| `follow_up(worker, prompt, timeout_secs?)` | `follow_up` | no | `StartResponse` for the new run. |
| `wait(WaitTargets, timeout_secs?, after_cursor?)` | `wait` | yes, bounded | `WaitResponse`: current states, `cursor`, `timed_out`, `unknown`, and `limit_reached` when the wait was not allowed to block. |
| `read(Target, Detail, after_cursor?, text_offset?)` | `read` | no | `ReadResponse`: the result, or progress. |
| `list()` | `list` | no | `ListResponse`: workers, limits and use. |
| `respond(interaction, decision)` | `respond` | no | `RespondResponse`. |
| `interrupt(Target)` | `interrupt` | yes, bounded | `InterruptResponse`: the result. |
| `close(worker)` | `close` | yes while it stops a running run, bounded | `CloseResponse`. |
| `shutdown()` | none | yes, bounded | `ShutdownReport`. |

- The MCP argument `after` is the library's `after_cursor`.
- A target is `Target::Run(id)` or `Target::Worker(id)`. A worker target means
  its current run, or its latest if nothing is current.
- `start` takes a `Request`. Merging a profile and overrides into one is the
  surface's job, with `PartialRequest`.
- `start` defaults a missing `cwd` to the embedder's working directory, as
  `delega run` does. `start_requiring_cwd` refuses a missing or relative `cwd`
  instead, kind `cwd_required`, as a finished run with no worker; `delega mcp`
  uses it. `follow_up` takes no `cwd` and runs in the first request's.
- `read` is compact by default; `Detail::Full` adds every field of the result,
  the retained activity history and the supervisor's diagnostics. `interrupt` and
  `close` return compact results.

A call that cannot be answered returns an `Error`:

| Error | When |
| --- | --- |
| `UnknownRun`, `UnknownWorker` | No run or worker has this id, or a finished run or closed worker has aged out ([Retention](#retention)). |
| `InvalidArguments` | The arguments do not fit together, such as `wait` with no ids. |
| `ShutDown` | The supervisor was shut down and takes no new work. |
| `Stopped` | The supervisor's task has ended. |
| `Failed` | The supervisor's own task failed. |

A run that cannot go ahead is not an error: it is a run that ends `refused`.

## States

A run is `queued`, `running` or `finished`. A finished run has one `RunResult`,
checked with `RunResult::validate` before it is stored. A result that fails the
check is a bug: it is replaced by a `failed` result of kind `internal_error`
whose error and `provider_notes` name the problems, and the supervisor goes on.

| Run from | to | When |
| --- | --- | --- |
| `queued` | `running` | a running slot is free and the run is next on its worker |
| `queued` | `finished` | interrupted, discarded by `close` or shutdown, or refused when its turn came |
| `running` | `finished` | the provider ended, the run was stopped, or its task failed |

`finished` is final. A worker is `queued`, `running`, `idle` or `closed`:

| Worker from | to | When |
| --- | --- | --- |
| (new) | `queued` or `running` | `start` admitted it |
| `queued` | `running` | its run was dispatched |
| `queued` | `idle` | its queued runs were all interrupted |
| `queued` | `closed` | `close` or shutdown |
| `running` | `queued` | its run finished and the next waits for a running slot |
| `running` | `idle` | its run finished and nothing is queued |
| `running` | `closed` | `close` or shutdown |
| `idle` | `queued` | `follow_up` while no running slot is free |
| `idle` | `running` | `follow_up` with a slot free |
| `idle` | `closed` | `close` or shutdown |

`closed` is final. A worker is `queued` when nothing is running and a run waits
for a slot; a worker with a run going and more behind it is `running`. While a
run has a pending permission request the worker reports `waiting_for_answer`
([Permission interactions](#permission-interactions)).

## Limits and defaults

The defaults are named constants in
[`supervisor/limits.rs`](https://github.com/joshrotenberg/delega/blob/main/crates/delega-core/src/supervisor/limits.rs).
`list` returns the limits in force with what is in use. `delega mcp` reads
lower values from the `[mcp]` section of the
[configuration file](api.md#configuration-file).

| Limit | Default | What a caller sees |
| --- | --- | --- |
| `max_live_workers` | 16 | A start beyond it is refused, kind `limit_reached`. A closed worker frees its slot at once. |
| `max_running_runs` | 4 | A run beyond it waits `queued`; it is not refused. Runs start in the order they became eligible. |
| `max_queued_per_worker` | 16 | A follow-up beyond it is refused, kind `queue_full`. With 0, an idle worker still takes a follow-up. |
| `retained_results` | 256 | Finished results kept readable. |
| `retained_bytes` | 64 MiB | Estimated size of finished results kept. Oldest dropped first; the newest is kept. |
| `retention` | 1 hour | How long a finished result stays readable. |
| `wait_ceiling` | 300 s | The longest one `wait` blocks. A longer `timeout_secs` is capped and the response says what applied. |
| `wait_default` | 30 s | `wait` with no `timeout_secs`. `list` reports it as `default_wait_secs`. |
| `max_waits_in_flight` | 64 | Waits blocked at once. Beyond it a wait returns at once with `limit_reached` ([Waiting and cursors](#waiting-and-cursors)). `list` reports `waits_in_flight`. The MCP server refuses a value above half its 128 concurrent requests. |
| preflight bound | 10 s | A bound on `start`'s dry run. A start that runs past it can be refused, kind `preflight_timeout`. |
| outstanding request checks | 16 | Checks running at once. A `start` beyond it waits for a slot within the preflight bound. Not in `list`. |
| compact answer | 20000 characters | A compact read returns at most this much of the answer. |
| compact structured answer | 16 KiB | A compact read leaves out a larger structured answer. |
| `shutdown_bound` | 30 s | The longest `shutdown` waits for runs to settle. |
| `interrupt_bound` | 30 s | The longest `interrupt` or `close` waits for a running run to settle. |

`SupervisorConfig::provider` is the base `ProviderOptions` every run inherits
(executable overrides, hook command, environment options). The supervisor sets
some fields per run, for example `run_id`, `worker` and `progress`
([`supervisor/config.rs`](https://github.com/joshrotenberg/delega/blob/main/crates/delega-core/src/supervisor/config.rs)).

## Starting and refusing

`start` checks the request, the name, the live-worker limit and a dry run of
the provider layer (working directory, `native_args`, executable, answer
schema), and can refuse the run with one of the kinds below. A slow filesystem
delays only that `start`, not other calls.

A start that is refused is still a run: the response is `finished`, the outcome
`refused`, the error says why, and `read` returns the result. **A refused start
creates no worker**, so the result has no `worker`.

| Kind | Why |
| --- | --- |
| `invalid_request` | The request fails the contract. |
| `name_in_use` | A live worker has this `name`. A closed worker's name is free again. |
| `limit_reached` | `max_live_workers` is reached. |
| `cwd_required` | `start_requiring_cwd` got no absolute `cwd`. |
| `preflight_timeout` | The checks did not finish within the preflight bound. |
| others | Whatever the provider layer's dry run refuses ([providers.md](providers.md#outcomes)). |

A `start` still being checked when `shutdown` begins returns `Error::ShutDown`,
or `Error::Failed` if the supervisor's task failed; neither creates a run.

An admitted run starts at once if a running slot is free. The provider layer
checks the request again when the run starts; a refusal there ends that run
`refused` and leaves the worker.

How the checks run: [`supervisor/preflight.rs`](https://github.com/joshrotenberg/delega/blob/main/crates/delega-core/src/supervisor/preflight.rs).

## Follow-ups

`follow_up` copies the worker's first request with the new prompt, and
`timeout_secs` if given, so permissions, working directory, model, instructions
and answer schema stay the same. The run queues behind the worker's other runs.
It resumes the worker's session (as `session.resume`), read when the run starts.

A worker does not switch session. Its session is the one it was started to
resume, or else the first `session_id` any of its runs reported. A later run
that reports a different id does not replace it, whatever its outcome
([providers.md](providers.md#outcomes)).

A follow-up is refused as a finished run, not an error:

| Kind | When |
| --- | --- |
| `worker_closed` | The worker is closed. |
| `queue_full` | The worker's queue is full. |
| `no_session` | The worker was started `ephemeral`, or has no session: none was given with `session.resume` and no run reported one. A follow-up can return `queued` and later end `no_session`. |
| `invalid_request` | The prompt is not valid. |

A response that says a run is `finished` carries its `outcome` and `error`. An
unknown worker is an error. `list` shows `can_follow_up`: false for a closed
worker, an `ephemeral` one, and one with no session and no run that might still
report one.

### Follow-up usage

The supervisor passes `usage` and `session_usage` through as the provider layer
reported them. A resumed Codex follow-up has no `usage`, and has
`session_usage` only when Codex reported totals. See [Usage](api.md#usage).

## Waiting and cursors

One counter numbers every finished run and every activity entry. A response
carries its value as `cursor`.

- `wait` returns the current states of what it names, after a change or its
  timeout, with a `cursor`. Pass the cursor back as `after_cursor` to wait for
  the next change. How waits are answered:
  [`supervisor/core/waits.rs`](https://github.com/joshrotenberg/delega/blob/main/crates/delega-core/src/supervisor/core/waits.rs).
- A timeout is a response with `timed_out: true` and the current states.
- Ids that name nothing are listed in `unknown`; the rest are still waited for.
  If all are unknown the call returns at once. No ids at all is
  `InvalidArguments`.
- A wait whose applied timeout is 0 is a poll: it returns the current states at
  once and never carries `limit_reached`.
- When `max_waits_in_flight` waits are already blocked, a wait that would block
  returns at once with the current states, `timed_out: true`, `timeout_secs: 0`
  and `limit_reached: "max_waits_in_flight"`. Its cursor is good for the next
  call. A wait with something to report is answered regardless.

## Reading

A finished run returns its result: compact by default (for example outcome,
text, structured answer, error, cleanup, session id and
[`applied`](api.md#applied)) or whole with `Detail::Full`. The fields are in
[`supervisor/views.rs`](https://github.com/joshrotenberg/delega/blob/main/crates/delega-core/src/supervisor/views.rs).

| Field | Meaning |
| --- | --- |
| `text_truncated`, `text_chars` | The compact answer was cut at 20000 characters; the full length. |
| `structured_omitted`, `structured_bytes` | The structured answer was over 16 KiB of JSON and left out; its size. `structured_valid` is still reported. |
| `text_window` | With `text_offset`: `offset`, `length`, `total` and `more`. |

**Paging a long answer.** Pass `text_offset` (characters) to get the window of up
to 20000 characters starting there, in either detail. Read again at
`offset + length` while `more` is true. An offset past the end gives an empty
window with the total. Page by run id: a `read` by worker returns its running
run, else its next queued run, else its latest finished one, which can change
between pages. The response names its `run`; treat a
different `run` as a new answer.

An unfinished run returns its state and progress: time queued and running, the
time limit, partial text and recent activity.

| Progress | Kept per run | Compact returns |
| --- | --- | --- |
| Partial text (the agent's messages so far, redacted, see [providers.md](providers.md#progress-events)) | last 16384 characters | last 2000 |
| Activity (provider events and delega's own entries, kind and short summary) | 200 entries | last 10 |

`after_cursor` returns only entries after it. Consecutive provider `system`
events with the same summary, such as Claude Code's `thinking_tokens`, are one
entry whose summary ends in ` (N in a row)`. The response carries
`activity_total` and `activity_omitted`.

Full detail adds the whole kept history and diagnostics: provider, permission
mode, queued and started times, whether and why a stop was requested, and how
many entries were dropped. The child process's own diagnostics are not
included; `cleanup` in the result is the process layer's verdict.

How activity is kept and counted: [`supervisor/activity.rs`](https://github.com/joshrotenberg/delega/blob/main/crates/delega-core/src/supervisor/activity.rs).

## Stopping

| Call | What happens |
| --- | --- |
| `interrupt` of a running run | Asks the provider to stop and waits up to `interrupt_bound`. `settled: true` with the result if it settled in time and the result is still kept; otherwise `settled: false` and no result. Runs queued behind it go on. |
| `interrupt` of a queued run | Removes it. Its result is `interrupted`: no `started_at`, zero duration, confirmed cleanup. |
| `interrupt` of a finished run | Returns its result unchanged. |
| `interrupt` of a worker | Acts on its current run. |
| `close` | Marks the worker closed, ends each queued run `interrupted` with `discarded by close`, stops the running run as `interrupt` does (it can return `settled: false`), and frees the live-worker slot at once; the running slot frees when the run ends. Finished results stay readable. Closing again reports `already_closed`, the current state and nothing discarded. |
| `shutdown` | Closes every live worker the same way with `discarded by shutdown`, refuses new work with `Error::ShutDown`, and waits up to `shutdown_bound`. The report's `unsettled` lists runs still running; `failed` reports a failure of the supervisor's task. It releases waits registered when it begins. Reads and `list` keep working. |
| Drop of the last clone | Signals every provider to stop and returns without waiting. The processes end shortly after. |

**Drop does not wait.** `shutdown().await` (or `connection_closed().await`,
the same bounded shutdown) waits up to `shutdown_bound`, and its report lists
runs still unsettled and any failure of the supervisor's task. An embedder such as an MCP server should call one and let it finish
before its runtime drops.

A panic in a run's task ends that run `failed`, kind `internal_error`, with an
unconfirmed cleanup; other workers are unaffected. A panic in the supervisor's
own task stops every provider; calls that need that task return
`Error::Failed`, and `shutdown()` returns a report with `failed` set.

How drop works: the `Drop` impl in
[`supervisor/mod.rs`](https://github.com/joshrotenberg/delega/blob/main/crates/delega-core/src/supervisor/mod.rs).

## Retention

Finished results are kept for `retention`, at most `retained_results` and
about `retained_bytes` (an estimate), oldest dropped first. A result past its time is not returned,
and an id that has gone is unknown. Live workers are not dropped, but their old
results are.

## Timing

`timing.queued_at` is when the supervisor admitted the run, so time in the
queue is `started_at` minus `queued_at`. `duration_ms` is the provider's run
from start to finish, and `timeout_secs` is the limit that applied; it does not
run while the run is queued.

### How the bounds count time

The applied `timeout_secs` of `wait` and the `interrupt_bound` of `interrupt`
and `close` are counted in awake time and checked against wall time about once
a second. After the machine slept past the limit, the call ends shortly after
it wakes and answers as it would on an ordinary timeout. A wall clock set back
counts as no time passed.

A run's own time limit counts awake time only: a run paused by sleep has not
used its time. The preflight bound, `shutdown_bound` and retention also count
awake time.

How the check works: [`supervisor/bound.rs`](https://github.com/joshrotenberg/delega/blob/main/crates/delega-core/src/supervisor/bound.rs).

## Ids

Worker and run ids are opaque strings, `worker-<tag>-<n>` and `run-<tag>-<n>`.
The tag differs between supervisors, so an id from another supervisor names
nothing here. Ids are not reused within a supervisor's life.

## Permission interactions

With Claude Code and `on_request: ask`, `wait` and `read` show
`pending_interactions`. The worker reports `waiting_for_answer`; its run stays
`running` and keeps its slot. Opening or resolving a request changes the cursor
for that run and worker.

- Call `respond` with the interaction id and a decision. The first decision wins.
- A permission timeout sends a deny, and the run can continue.
- `interrupt` denies pending requests and stops the run; the worker stays
  available.
- `close`, shutdown and transport closure deny pending requests and close the
  worker.
- The run's time limit includes time spent waiting for an answer.

## Not built

- Persisting workers across a restart.
- Review layers: roles, owners and barriers on a worker.
- Process diagnostics from the child in a full read.
- A timer for retention. Pruning happens when a call arrives, so memory is
  bounded by the count, not released on a clock while nothing is called.

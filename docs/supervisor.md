# The supervisor

`delega_core::supervisor` runs several workers at once. It is the library
behind the MCP tools of [api.md](api.md#mcp): each tool is one operation of
[`Supervisor`](../crates/delega-core/src/supervisor/mod.rs), with the same
arguments and a serializable response. The MCP server is a later change; it maps
tool arguments to these calls and returns the responses as they are.

A worker is one provider conversation. A run is one assignment given to it.
`start` makes a worker and its first run; `follow_up` adds a run to the same
worker, which continues the worker's provider session.

## Operations

| Operation | MCP tool | Waits for a provider? | Returns |
| --- | --- | --- | --- |
| `start(Request)` | `start` | no | `StartResponse`: worker id, run id, state `queued` or `running`. |
| `follow_up(worker, prompt, timeout_secs?)` | `follow_up` | no | `StartResponse` for the new run. |
| `wait(WaitTargets, timeout_secs?, after_cursor?)` | `wait` | yes, bounded | `WaitResponse`: current states, a cursor, `timed_out`, `unknown`. |
| `read(Target, Detail, after_cursor?)` | `read` | no | `ReadResponse`: the result, or progress. |
| `list()` | `list` | no | `ListResponse`: workers, limits and use. |
| `interrupt(Target)` | `interrupt` | yes, bounded | `InterruptResponse`: the result. |
| `close(worker)` | `close` | yes while it stops a running run, bounded | `CloseResponse`. |
| `shutdown()` | none | yes, bounded | `ShutdownReport`. |

The MCP argument `after` is the library's `after_cursor`. `start` takes a
`Request`; merging a profile and overrides into one is the surface's job, with
`PartialRequest`. A target is `Target::Run(id)` or `Target::Worker(id)`; for a
worker it means the worker's current run, or its latest if nothing is current.

Every response is compact by default. `Detail::Full` adds every field of the
result, the whole activity history and the supervisor's own diagnostics. An
operation that cannot be answered at all returns an `Error`: an unknown run or
worker, arguments that do not fit together, a supervisor that was shut down. A
run that cannot go ahead is never an error: it is a run that ends `refused`.

## States

A run is `queued`, `running` or `finished`. A finished run has exactly one
`RunResult`, built by the contract's constructors and checked with
`RunResult::validate` before it is stored. That covers the results the provider
layer returns and every one the supervisor builds itself: refused (at admission
and for an invalid request), discarded, interrupted while queued, a task that
failed, and the replacement for a result that failed the check. None is skipped. A result that fails the check is
a bug: a debug build fails loudly, and a release build replaces it with a
`failed` result of kind `internal_error` that says so.

| Run from | to | When |
| --- | --- | --- |
| `queued` | `running` | a running slot is free and the run is next on its worker |
| `queued` | `finished` | interrupted, discarded by `close` or shutdown, or refused when its turn came |
| `running` | `finished` | the provider ended, or the run was stopped, or its task failed |

`finished` is final. A worker is `queued`, `running`, `idle` or `closed`. Its
state is derived from what it holds (closed, a running run, queued runs) and
applied through a table that rejects any other change:

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

`closed` is final. `queued` means the worker has nothing running and a run
waiting for a running slot; a worker with a run going and more behind it is
`running`.

## Limits and defaults

All defaults are named constants in `supervisor/limits.rs`.

| Limit | Default | Meaning |
| --- | --- | --- |
| `max_live_workers` | 16 | Live workers at once. A closed worker frees its slot at once. A start beyond it is refused, kind `limit_reached`. |
| `max_running_runs` | 4 | Runs running at once. A run beyond it waits `queued`; it is not refused. Runs start in the order they became eligible, so one busy worker cannot starve another. |
| `max_queued_per_worker` | 16 | Runs waiting behind a worker's running run. A follow-up beyond it is refused, kind `queue_full`. 0 means no waiting behind a running run: an idle worker still takes a follow-up, which starts at once. |
| `retained_results` | 256 | Finished results kept readable, across the supervisor. |
| `retained_bytes` | 64 MiB | Estimated bytes of finished results kept (answer, structured answer, notes and partial text, plus a fixed amount each). The oldest are dropped first; the newest is always kept. |
| `retention` | 1 hour | How long a finished result stays readable. |
| `wait_ceiling` | 300 s | The longest one `wait` blocks. A longer `timeout_secs` is capped and the response says what applied. |
| default wait | 30 s | Used when `wait` gets no `timeout_secs`. The ceiling is kept high for library callers; the MCP layer should keep each wait short, well inside what a tool-calling client allows for one call, and rely on the cursor to continue. |
| preflight bound | 10 s | The longest `start` spends checking paths, the executable and the schema before it is admitted. |
| compact answer | 20000 characters | A compact read returns at most this much of the answer; `text_offset` pages through the rest. |
| compact structured answer | 16 KiB | A compact read leaves out a structured answer whose JSON is larger than this and gives its size. |
| `shutdown_bound` | 30 s | The longest `shutdown` waits for runs to settle. |
| `interrupt_bound` | 30 s | The longest `interrupt` or `close` waits for a running run to settle. |

The process layer bounds each stop on its own by its cleanup deadline, so the
supervisor's bounds only matter when a stop is slower than that. `list` returns
the limits with what is in use. `SupervisorConfig::provider` is the base
`ProviderOptions` every run inherits: executable overrides, the hook command and
the environment options. For each run the supervisor sets `run_id`, `worker`,
`progress` and `progress_text` itself.

## Starting and refusing

`start` validates the request, checks the name, checks the live-worker limit and
reports what the provider layer's dry run found, which checks the working
directory, the `native_args`, the executable and the answer schema. The dry run
touches the filesystem, so it is not done on the supervisor's task: the caller's
`start` runs it on a thread of its own, bounded by the preflight bound, and posts
the result to the supervisor, so a hung filesystem can delay only that `start`,
never `wait` or `interrupt`. If the bound passes the start is refused (kind
`preflight_timeout`). The thread is detached and cannot be cancelled: nothing
waits for it, so a hung filesystem cannot hold up `shutdown` or a runtime drop,
and its late result is discarded when the filesystem answers. If any step fails the
request is still a run: the response is `finished`, the outcome is `refused`, the
error says why, and `read` returns the result. **A refused start creates no
worker**, so the result has no `worker`, as the contract says. Names are unique
among live workers, so a second live worker with the same `name` is refused,
kind `name_in_use`; a closed worker's name is free again.

A run that passes admission is queued on its new worker and starts at once if a
running slot is free. The provider layer checks the request again when the run
starts; a refusal there ends that run `refused` and leaves the worker.

## Follow-ups

`follow_up` copies the worker's first request with the new prompt, and
`timeout_secs` if given, so permissions, working directory, model, instructions
and answer schema are the same. The run is queued behind the worker's running
and queued runs and runs when its turn comes. The session to continue is the
latest `session_id` any run of the worker reported, read when the run starts, so
a follow-up queued behind a run that has not finished still resumes what that
run reports. It is passed as `session.resume`.

A follow-up is refused as a finished run, not an error, when:

- the worker is closed, kind `worker_closed`;
- its queue is full, kind `queue_full`;
- the worker was started `ephemeral`, so there is no session to continue, kind
  `no_session`;
- the prompt is not valid, kind `invalid_request`.

A follow-up queued behind a run that ended without reporting a session is
refused, kind `no_session`, when its turn comes. If the worker is idle when the
follow-up is made, the refusal is immediate and the response says so: every
response that says a run is `finished` carries its `outcome` and its `error`
(kind and message). An unknown worker is an error.

A worker whose first run ended without reporting a session, such as one whose
provider failed before it started, cannot take a follow-up. `list` flags it:
`can_follow_up` is false for a closed worker, an `ephemeral` one, and one that
has no session and no run that might still report one.

## Waiting and cursors

One counter numbers every run that finishes and every activity entry. A response
carries its value as `cursor`.

- Without `after_cursor`, `wait` returns when any named run is finished, or when
  a named worker's current run, the one it had when the call was made, is
  finished. A worker with nothing running has nothing to wait for and returns at
  once.
- With `after_cursor`, it returns when any named run, or any run of a named
  worker, finished after that cursor, at once if one already has. Passing back
  the cursor of the previous response sees every finish once and never misses
  one. Each run and worker in the response says whether it `changed` after the
  cursor.
- A timeout is a response with `timed_out` true and the current states.
- An id that names nothing is listed in `unknown`; the known ones are still
  waited for, and a call whose ids are all unknown returns at once.
- A call with no ids is an `InvalidArguments` error.

The wait itself happens in the caller's task. The supervisor's task registers
the wait and later builds the answer from current states, so it never blocks.

## Reading

A finished run returns its result: compact by default (run, worker, provider,
outcome, text, structured answer, error, cleanup, denied requests, provider
notes, session id, timing) or whole with `Detail::Full`. Compact returns the
first 20000 characters of the answer; a longer answer sets `text_truncated` and
`text_chars` gives its full length. A structured answer larger than 16 KiB of
JSON is left out of the compact form: `structured_omitted` is true and
`structured_bytes` gives its size; `structured_valid` is still reported. `detail:
full` returns both whole. `interrupt` and `close` return the compact form.

**Paging a long answer.** `read` takes `text_offset`, a number of characters. The
result's text is then the window of up to 20000 characters starting there, in
either detail (a full read without an offset returns everything), and
`text_window` gives `offset`, `length`, the `total` characters in the answer and
`more`. Read again with `offset + length` while `more` is true. An offset past the
end gives an empty window with the total. For the MCP layer, `text_offset` is a
plain argument of `read`; a client reads a long answer one window per call. An unfinished run
returns its state and progress: time queued and running, the time limit, the
partial text, and recent activity.

Partial text is what the agent has written so far, taken from the provider
layer's `ProgressEvent::Message` events (see
[providers.md](providers.md#progress-events)); it is redacted, bounded to the
last 16384 characters, and compact returns the last 2000. Activity is one entry
per provider output line with a kind and a short summary, an agent message shown
by its length; the history keeps 200 entries and counts what it dropped, and
compact returns the last 10. `after_cursor` returns only entries after it.

Full detail adds the whole kept history and diagnostics: provider, permission
mode, queued and started times, whether and why a stop was requested, and how
many entries were dropped. A full read of a finished run also returns its
activity history. The provider layer does not expose the child process's own
diagnostics, so none are included; `cleanup` in the result is the process layer's
verdict.

## Stopping

- **interrupt of a running run** asks the provider layer to stop it and waits,
  bounded, for it to settle. The result is `interrupted` and carries the
  process layer's cleanup verdict, the partial text and the session id. Runs
  queued behind it go on.
- **interrupt of a queued run** removes it. Its result is `interrupted`, never
  started: no `started_at`, zero duration, confirmed cleanup.
- **interrupt of a finished run** returns its result unchanged.
- **interrupt of a worker** acts on its current run.
- **close** marks the worker closed, finishes every queued run `interrupted`
  with the message `discarded by close`, stops the running run and waits for it,
  and frees the live-worker slot at once. The running-run slot is freed when the
  run has stopped. Finished results stay readable. Closing a closed worker
  returns the same answer with `already_closed`. An outcome of `interrupted`
  was chosen because the caller stopped the run; `refused` is for a run that did
  not go ahead on its own terms.
- **shutdown** closes every live worker the same way, with the message
  `discarded by shutdown`, refuses new work with `Error::ShutDown`, and waits for
  runs to settle within the bound, naming any that did not. Reads and `list`
  keep working.
- **Drop** of the last clone of a `Supervisor` signals everything and returns.
  It cancels one token that is the parent of every run's stop token, so every
  provider is signalled at once without the supervisor's task having to run,
  and it asks that task to close every worker. It does nothing else: it never
  waits, it needs no runtime handle, and it cannot panic, so it is safe inside a
  spawned task, inside a `LocalSet`, on the thread that called `block_on`, on a
  current-thread runtime, on a plain thread, while unwinding from a panic, and
  while the runtime itself is being dropped.

  What it does not do is confirm anything. The processes are gone shortly after
  the drop, not by the time it returns: each run's task interrupts its provider
  when it is next polled, and if the runtime is dropped first, the process
  layer's own drop path kills each child's process group and its tracked
  descendants without waiting for a process listing. The tests drop with ten
  workers running in each of those places, require the drop to return within 5 s
  (the old wait it replaced was 32 s), and poll until no provider process is
  left.

  **Only `shutdown().await` waits for cleanup and reports what could not be
  confirmed.** An MCP server must call it, and let it finish, before its runtime
  drops; drop is the safety net for when it did not, not a way to stop workers.

A panic in the task that follows a run ends that run `failed`, error kind
`internal_error`, with an unconfirmed cleanup that says what could not be
checked. The supervisor and every other worker are unaffected.

A panic in the supervisor's own task stops every provider on the way out (the
state it owned is dropped, and dropping it signals every run), and from then on
every call returns `Error::Failed`, and `shutdown()` returns a report with
`failed` set that says nothing confirmed any provider ended. It is never a clean
report.

## Retention

Finished results are kept for `retention`, at most `retained_results` at a
time and at most `retained_bytes` estimated bytes, oldest dropped first. Pruning runs before each command, so a read never
returns a result past its time. A closed worker is listed while any of its
results is kept and then disappears, also when none was kept by the time it was
closed; an id that has gone is reported as unknown. Live workers are never
dropped, but their old results are.

## Timing

`timing.queued_at` is when the supervisor admitted the run, so the time spent in
the queue is in the result: `started_at` minus `queued_at`. `duration_ms` is the
provider's run from start to finish, and `timeout_secs` is the limit that applied
to it; the limit does not run while the run is queued.

## Ids

Worker and run ids are opaque strings, `worker-<tag>-<n>` and `run-<tag>-<n>`.
The tag differs between supervisors, so an id from another supervisor does not
name anything here. Ids are unique for the supervisor's life and never reused.

## Tests

The tests are in `crates/delega-core/tests/supervisor/`, a test target of their
own that uses only the public API, plus a few unit tests in
`src/supervisor/tests.rs` for the activity buffer and for a run's task that
panics. They run the fake providers in `provider/tests/fakes`, copied per test
into a directory whose path is in every process's command line. At most three
run at once, and `cargo test` runs the target apart from the library's unit
tests, because many shell processes polled in parallel starved the timing tests
of the process layer on a busy machine. Every test that starts a provider ends by checking the process table for any
process that carries that path. The concurrency limit of a batch is measured
from outside, by the fakes' own records (`trace:` runs), not by the supervisor's
counter. A run is held open by
a gate (`gate:FILE`), so the order of events depends on files, not on timing. The
tests cover every operation, the state tables, queue order, resuming a session,
limits reached and freed, retention, wait with and without a cursor, on timeout,
with several runs and with unknown ids, reads during a run, interrupt of a
running, queued and finished run, close with a running run and a queue, shutdown
with ten workers running, drop of the last clone in eight places (inside a
spawned task with four runtime threads and with one, on the `block_on` thread,
inside a `LocalSet`, while unwinding from a panic, on a current-thread runtime,
from a plain thread with no runtime, and when the runtime is dropped while a task
holds the last clone), paging and the compact bounds, a
panic in a run's task and one in the supervisor's own task, and fifty runs
across ten workers.

## Not built

- The MCP server and `delega mcp`.
- Persisting workers across a restart.
- `waiting_for_answer`, `respond` and `permissions.asked`, which depend on a
  long-lived provider session.
- Review layers: roles, owners and barriers on a worker.
- Process diagnostics from the child in a full read.
- A timer for retention. Pruning is lazy, so memory is bounded by the count, not
  released on a clock while nothing is called.

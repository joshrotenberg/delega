# Outcomes and cleanup

A run ends in one of five outcomes and produces one result. The result also
reports whether delega could confirm that the processes it started are gone.

## Outcomes

| Outcome | Meaning |
| --- | --- |
| `completed` | The provider finished and returned a final answer. |
| `failed` | The provider or delega hit an error. |
| `timed_out` | The run's time limit passed. |
| `interrupted` | The caller stopped the run. |
| `refused` | The run did not go ahead, or stopped because a permission it needed was denied. |

`completed` says the provider finished, not that the work is correct. The rules
for each outcome are in [Outcomes](../api.md#outcomes) and, per provider, in
[Outcomes](../providers.md#outcomes). For `delega run`, the exit status follows
the outcome ([Command line](../api.md#command-line)).

## Error kinds

A result that is not `completed` carries an `error` with a `kind` and a
`message`. The provider layer, the CLI and the supervisor each add checks of
their own, and their kinds are listed in
[Error kinds](../providers.md#error-kinds) and [Outcomes](../api.md#outcomes).

## Cleanup

`cleanup.confirmed: true` means delega saw no process left that it started. A
run that started no process, such as a refusal or a run interrupted while
queued, can be confirmed with no process at all. `confirmed: false` comes with
the pids, process groups and a reason.

Confirmed is not a guarantee. The process layer documents gaps where a
descendant can outlive a confirmed run, in
[`crates/delega-core/src/process/mod.rs`](https://github.com/joshrotenberg/delega/blob/main/crates/delega-core/src/process/mod.rs),
and the [acceptance harness](../acceptance.md) tests them
([matrix](https://github.com/joshrotenberg/delega/blob/main/acceptance/README.md#the-matrix),
[expectations](https://github.com/joshrotenberg/delega/blob/main/acceptance/README.md#expectations)).
On macOS the keeper cannot adopt orphans, and a descendant that cleanup does
not reach can be missed. A kept run on macOS says so in `provider_notes`. The
scope is in "What a macOS verdict covers" in
[`process/mod.rs`](https://github.com/joshrotenberg/delega/blob/main/crates/delega-core/src/process/mod.rs).
Under the supervisor, `interrupt`, `close` and `shutdown` wait for a run to
settle within a bound ([Stopping](../supervisor.md#stopping)).

## Reading a result

Read `outcome`, then `error` when it is not `completed`, then
`cleanup.confirmed`. `permissions.denied` and `provider_notes` say what was not
granted and where the provider differed from the request. Then check the work
itself. Every field is in [The result](../api.md#the-result).

# Outcomes and cleanup

Every run ends in one of five outcomes and produces one result. The result also
says whether delega could confirm that the provider's processes are gone.

## The five outcomes

| Outcome | Meaning |
| --- | --- |
| `completed` | The provider finished the assignment and returned a final answer. |
| `failed` | The provider or delega hit an error. `error.kind` says which. |
| `timed_out` | The run's time limit passed. |
| `interrupted` | The caller stopped it, or the caller went away. |
| `refused` | The run did not go ahead: the request was invalid, a limit was reached, or a permission the run needed was denied and it could not continue. |

`completed` says the provider finished. It does not say the work is correct;
checking that belongs to the caller. The conditions each provider must meet for
each outcome are in [Outcomes](../providers.md#outcomes), and the contract's
table is in [Outcomes](../api.md#outcomes).

A run that was denied a permission and still returned a final answer is
`completed`, with the denial in `permissions.denied`
([Permissions](permissions.md#requests-that-are-not-granted)).

For `delega run`, the exit status follows the outcome; the codes are under
"Exit status" in [Command line](../api.md#command-line).

## Error kinds

Every outcome other than `completed` carries an `error` with a `kind` and a
`message`, and `provider_exit_code` when the provider process exited with one.
The kinds are listed in [Error kinds](../providers.md#error-kinds), grouped by
the outcome they belong to. The supervisor, `delega run` and `delega mcp` add
kinds of their own, listed in [Outcomes](../api.md#outcomes). When a failure
says why it happened, such as a usage limit or an authentication problem, it
gets a kind for the reason
([Provider failure kinds](../providers.md#provider-failure-kinds)).

A refusal before start reports the first problem found, in a fixed order given
in [Permissions](../api.md#permissions).

## Cleanup

When a run ends, delega's process layer sweeps for what the provider left
running: processes in its process group, in its session, and descendants it
tracked. The result's `cleanup` holds the verdict.

- `confirmed: true` means a final sweep after the provider exited found nothing
  left in those places.
- `confirmed: false` comes with `pids`, `process_groups` and a `reason` that
  says what could not be confirmed.

`confirmed: true` does not mean every process the provider started has ended.
There are two documented gaps where it can be reported while a descendant still
runs: a descendant that calls `setsid` and whose parent exited before any
process listing saw them together, and a fast fork relay. Both are described,
with the conditions and what would close them, in
[`crates/delega-core/src/process/mod.rs`](https://github.com/joshrotenberg/delega/blob/main/crates/delega-core/src/process/mod.rs)
under "What `Confirmed` means" and "What remains possible".

The [acceptance harness](../acceptance.md) runs `delega run` against fake
providers and checks what was really left running against what the result
said. Its [matrix](https://github.com/joshrotenberg/delega/blob/main/acceptance/README.md#the-matrix) lists the cases, and
[Expectations](https://github.com/joshrotenberg/delega/blob/main/acceptance/README.md#expectations) lists the known gaps the harness
expects to see. Rows that force an unconfirmed cleanup are described in
[Forcing an unconfirmed cleanup](https://github.com/joshrotenberg/delega/blob/main/acceptance/README.md#forcing-an-unconfirmed-cleanup).

Under the supervisor, only `shutdown` waits for cleanup and reports what could
not be confirmed. Dropping the supervisor signals every run and does not wait
([Stopping](../supervisor.md#stopping)).

## Reading a result

A caller that only needs to know what happened reads `outcome`, `text`, `error`
and `cleanup` ([The result](../api.md#the-result)). In practice:

1. Read `outcome`. For anything other than `completed`, read `error.kind` and
   `error.message`. `text` can hold a partial answer.
2. Read `cleanup.confirmed`. When it is `false`, the `pids`, `process_groups`
   and `reason` say what may still be running.
3. Read `permissions.denied` and `provider_notes` for what was not granted and
   where the provider did something other than what was asked.
4. With an answer schema, check `structured_valid` before using `structured`.
5. Check the work itself. `completed` is the provider's report that it
   finished, not a verification.

The whole result, field by field, is in [The result](../api.md#the-result).

# First run

`delega run` runs one assignment in the foreground and exits. Run it from the
directory the worker should work in, or name one with `-C`.

## See what would run

`--dry-run` validates the request and prints the provider executable, its
arguments, the working directory, the time limit, the environment variable
names a worker would get and the permissions applied, without starting
anything, not even a version probe:

```sh
delega run --provider codex -p "Summarize this repository" --dry-run
```

It lists environment variable names, never values, and replaces the assignment
by its size. A request that would be refused prints the refusal and exits 2; a
dry run exits 0 otherwise. `--dry-run --output json` prints the same as one JSON
document ([Command line](../api.md#command-line), "Dry run").

## Run one assignment

```sh
delega run --provider codex -p "Summarize this repository"
```

The worker runs with read-only permissions (the default) and a time limit of 600
seconds unless `--timeout` sets another. When the run completes, the answer is
printed on standard output and nothing else is.

## Reading the result

The exit status follows the outcome:

| Status | Outcome |
| --- | --- |
| 0 | `completed` |
| 1 | `failed` |
| 2 | `refused`, and usage errors |
| 124 | `timed_out` |
| 130 | `interrupted` |

These hold when the result is delivered normally. A result that cannot be
written, and a write cut short by a signal, are covered under
[Command line](../api.md#command-line).

With text output, standard output holds the answer only for a `completed` run.
For any other outcome it stays empty, and standard error gets one line:

```text
delega run: <outcome>: <error.kind>: <error.message>
```

Each denied request is written to standard error as `delega run: denied:
<request>`, whatever the outcome, and cleanup that could not be confirmed as
`delega run: cleanup not confirmed: ...`. A run that did not complete gives its
reason in `error.kind` ([Error kinds](../providers.md#error-kinds)).

## The whole result as JSON

```sh
delega run --provider codex -p "Summarize this repository" --output json
```

`--output json` prints the whole result as one JSON document, whatever the exit
status. A caller that only needs to know what happened reads `outcome`, `text`,
`error` and `cleanup`. `provider_notes` says where the provider did not do what
the request asked, or did it differently, and `session_id` is what `--resume`
takes. Every field is described under [The result](../api.md#the-result), and
`delega schema result` prints the result's JSON Schema.

## More forms

```sh
delega run --provider claude --stdin --output json < task.txt > result.json
delega run --provider codex --write --timeout 20m -p "Fix the failing test"
delega run --provider claude --allow-command "cargo test" -p "Run the tests and report"
delega run --provider codex --resume SESSION_ID -p "Now add tests"
delega run --provider claude -p "Plan the change" --dry-run
```

- `--stdin` reads the assignment from standard input.
- `--write` allows editing files in the working directory, and `--full` removes
  delega's restrictions. `--allow-command` grants one shell command, matched
  exactly. Codex confines neither reads nor commands, so a `read` or `write`
  Codex run is wider than requested and says so in `provider_notes`; `--strict`
  refuses it instead ([Permissions](../api.md#permissions),
  [Wider than requested](../providers.md#wider-than-requested)).
- `--timeout` takes `90s`, `20m`, `2h` or a plain number of seconds.
- `--resume` continues a saved provider session by its `session_id`.

`delega run --help` lists every option. [Command line](../api.md#command-line)
describes profiles (`--request`), `--progress`, `--env` and the
[configuration file](../api.md#configuration-file).

## Next

- [Plugin setup](plugin.md), to supervise several workers from a Claude Code or
  Codex session.
- [Status and limits](../project/status.md), for what permissions and cleanup do
  not cover yet.

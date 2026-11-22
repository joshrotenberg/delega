# Run from a script or CI

`delega run` runs one assignment in the foreground and exits with a status that
follows the outcome. That makes it usable from a shell script or a CI job. The
machine must have the provider installed and signed in; check it first with
[`delega doctor`](doctor.md). The full reference is
[Command line](../api.md#command-line) and the
[`run` skill](https://github.com/joshrotenberg/delega/blob/main/skills/run/SKILL.md).

## Exit statuses

| Status | Outcome |
| --- | --- |
| 0 | `completed` |
| 1 | `failed` |
| 2 | `refused`, including usage errors |
| 124 | `timed_out` |
| 130 | `interrupted` |

## A script

```sh
#!/bin/sh
delega run --provider claude --read -C /path/to/repo --timeout 20m \
  --stdin --output json < task.txt > result.json
status=$?

case $status in
  0) jq -r .text result.json ;;
  124) echo "timed out" >&2; exit 1 ;;
  *) jq -r '.error.kind + ": " + .error.message' result.json >&2; exit 1 ;;
esac

jq -e '.cleanup.confirmed' result.json > /dev/null || {
  echo "cleanup not confirmed" >&2
  exit 1
}
```

With `--output json` the whole result is printed as one JSON document on
standard output, whatever the exit status, so a script can always read
`outcome`, `error` and `cleanup` from it. Usage errors that the argument parser
rejects (an unknown flag, for example) exit 2 with a message and no result.

With the default `--output text`, standard output holds the answer only when the
run completed, and the outcome and error go to standard error as one line.

## Profiles

A profile is a partial request saved as JSON. Flags given on the command line
replace its fields, and lists replace, not extend:

```json
{
  "schema_version": 1,
  "provider": "codex",
  "timeout_secs": 1200,
  "instructions": "Answer in plain text. Do not edit files.",
  "permissions": { "mode": "read" }
}
```

```sh
delega run --request profile.json -C /path/to/repo --stdin --output json < task.txt
```

`instructions` has no flag and comes only from a profile. Run with `--dry-run`
to see the merged request, the provider arguments and the environment variable
names without starting anything. See
[The request](../api.md#the-request) for every field.

## Permissions and environment

- Use the narrowest mode: `--read` (the default), `--write` or `--full`, and
  `--allow-command CMD` for exact commands. See
  [Permissions](../api.md#permissions).
- A worker does not inherit the job's environment. Pass what it needs with
  `--env NAME`, which passes the variable's value without putting it on the
  command line. See [Environment policy](../providers.md#environment-policy).
- A user configuration file can set defaults; `--dry-run` and the result's
  `applied.sources` say which layer set each field
  ([`applied`](../api.md#applied)).

## Stopping

Stop `delega` with SIGTERM or SIGINT. It stops the worker, prints the result and
exits 130. SIGKILL cannot be handled and leaves the worker running with no time
limit enforced, so a CI job's cancel step should send SIGTERM first.

## What to check

- The exit status.
- `cleanup.confirmed`.
- `permissions.denied` and `provider_notes`: a run can be `completed` with
  denied requests.
- After `--write` or `--full`, the changes themselves, with your own tests.

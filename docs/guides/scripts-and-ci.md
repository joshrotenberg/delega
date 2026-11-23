# Run from a script or CI

`delega run` runs one assignment in the foreground and exits with a status that
follows the outcome. That makes it usable from a shell script or a CI job. The
machine must have the provider installed and signed in. [`delega doctor`](doctor.md)
checks the executable and the local prerequisites; it does not check that the
provider is signed in. The full reference is
[Command line](../api.md#command-line) and the
[`run` skill](https://github.com/joshrotenberg/delega/blob/main/skills/run/SKILL.md).

## Exit statuses

The common ones: 0 `completed`, 1 `failed`, 2 `refused` (including usage
errors), 124 `timed_out`, 130 `interrupted`. The status and the result do not
always line up one to one: an argument the parser rejects exits 2 with nothing
on standard output, a write that fails or is cut short can leave an incomplete
result, and a signal that arrives after the provider finished can keep the
completed status. See the **Exit status** and **Writing the result** entries
under [Command line](../api.md#command-line).

## A script

```sh
#!/bin/sh
delega run --provider claude --read -C /path/to/repo --timeout 20m \
  --stdin --output json < task.txt > result.json
status=$?

if ! jq -e 'type == "object" and has("outcome")' result.json > /dev/null 2>&1; then
  echo "no complete result (exit status $status)" >&2
  exit 1
fi

if [ "$status" -ne 0 ]; then
  jq -r '"\(.outcome): \(.error.kind // "none"): \(.error.message // "")"' result.json >&2
  exit 1
fi

if ! jq -e '.outcome == "completed" and .cleanup.confirmed == true' result.json > /dev/null; then
  echo "outcome not completed or cleanup not confirmed" >&2
  exit 1
fi

jq -r .text result.json
```

Standard error is left alone, so the CLI's own lines (the outcome and error of a
run that did not complete, denied requests, cleanup that could not be confirmed)
reach the job log.

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
to see the launch plan and where each setting came from, without starting
anything; it leaves out the prompt and instruction text. See
[The request](../api.md#the-request) for every field.

## Permissions and environment

- Use the narrowest mode: `--read`, `--write`, `--build` or `--full`, and
  `--allow-command CMD` for exact commands in `--read` and `--write`. See
  [Permissions](../api.md#permissions).
- A worker does not inherit the job's environment. Pass what it needs with
  `--env NAME`, which passes the variable's value without putting it on the
  command line. See [Environment policy](../providers.md#environment-policy).
- A user configuration file can set defaults; `--dry-run` and the result's
  `applied.sources` say which layer set each field
  ([`applied`](../api.md#applied)).

## Stopping

Stop `delega` with SIGTERM or SIGINT, which let it stop the worker and clean
up. The result can be missing or incomplete if the signal interrupts writing
it; the script above already handles that. SIGKILL cannot be handled and leaves the worker running with no
time limit enforced, so a CI job's cancel step should send SIGTERM first.

## What to check

- The exit status, and that the result file is a complete JSON object.
- `cleanup.confirmed`.
- `permissions.denied` and `provider_notes`: a run can be `completed` with
  denied requests.
- After `--write`, `--build` or `--full`, the changes themselves, with your own tests.

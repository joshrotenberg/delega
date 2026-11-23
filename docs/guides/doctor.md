# Check provider readiness

`delega doctor` reports what a run on this machine would meet, without starting a
worker and without writing anything. Run it before the first run on a new
machine or CI image, and when a run is refused because a provider is missing, a
version is too old or a settings file cannot be used. It does not check that a
provider is signed in. The full reference is [Doctor](../api.md#doctor).

## Run it

```sh
cd /path/to/repo
delega doctor
delega doctor --output json
```

Run it in the directory the workers will use, because the checks are those of a
run in the current directory. It has no `--cwd` and no `--env`. Where Claude
Code deny rules are read from is in [Deny rules](../providers.md#deny-rules).

## What it reports

For each provider: the executable a run would use and where it came from, its
version, counts of the environment a worker would get, the files a Claude run
would read deny rules from, with their rule counts (not the rules), and managed settings found.
For Claude Code it also says whether the sandbox `build` runs need can start. Also the configuration file, the
limits in force, the built-in skills, and whether process inspection works. It
never prints an environment value, a deny rule or a file's content. See
[Doctor](../api.md#doctor) for each field.

## Exit status and readiness

| Status | Meaning |
| --- | --- |
| 0 | A run can go ahead. |
| 1 | No provider is ready, the configuration file is refused, or process inspection is unavailable. |
| 2 | Doctor itself cannot run. |

The top-level `ready` matches the exit status. A provider entry can be
`ready: true` while doctor exits 1, for example when the configuration file is
refused or process inspection is unavailable, and a missing provider does not
make the exit 1 when the other one is ready. To gate a CI job on one provider,
check all three and fail closed:

```sh
delega doctor --output json > doctor.json
status=$?
if [ "$status" -ne 0 ]; then
  echo "delega doctor exited $status" >&2
  exit 1
fi
if ! jq -e '.ready == true and any(.providers[]; .provider == "claude" and .ready == true)' doctor.json > /dev/null 2>&1; then
  echo "claude is not ready" >&2
  exit 1
fi
```

The document's JSON Schema is
[`crates/delega-core/schemas/doctor.schema.json`](https://github.com/joshrotenberg/delega/blob/main/crates/delega-core/schemas/doctor.schema.json).

## Reading problems

Each entry in `problems` has a `kind`, a message, the provider (null for the
whole system) and what it `blocks`: `run`, `command_grants`, `build`, `cleanup`
or `nothing`. A provider is `ready` when no problem blocks `run` for it. The table
of kinds is in [Doctor](../api.md#doctor), and the managed settings locations
are in [Managed settings](../providers.md#managed-settings).

## Before using build with Claude Code

Claude Code's `build` runs need its sandbox. On macOS it is built in. On Linux
it needs `bwrap` and `socat` on the `PATH` the worker gets. When one is
missing, doctor shows:

```text
claude: ready
  ...
  sandbox for build runs: not available, missing socat
  ...
problems:
  claude: sandbox_unavailable (stops build runs): Claude Code's sandbox needs bwrap and socat on PATH and socat was not found, so every build run with Claude would fail at startup; install socat
```

Claude stays `ready` for other modes and the exit status does not change.
Install the `bubblewrap` and `socat` packages with the system's package
manager, then run doctor again. In JSON, read `providers[].sandbox.missing`
or look for a problem with `blocks: "build"`.

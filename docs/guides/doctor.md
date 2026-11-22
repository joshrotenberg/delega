# Check provider readiness

`delega doctor` reports what a run on this machine would meet, without starting a
worker and without writing anything. Run it before the first run on a new
machine or CI image, and when a run is refused because a provider is missing, a
version is too old or a settings file cannot be used. The full reference is
[Doctor](../api.md#doctor).

## Run it

```sh
cd /path/to/repo
delega doctor
delega doctor --output json
```

Run it in the directory the workers will use: the Claude Code deny rules it
reports come from the user's settings and the current directory's
`.claude/settings.json` and `settings.local.json`. It has no `--cwd` and no
`--env`; it applies what the [configuration file](../api.md#configuration-file)
adds, because a run would. The only provider process it starts is each
provider's `--version` probe.

## What it reports

For `claude` and `codex` each:

- the executable a run would use and where it came from (`DELEGA_CLAUDE_BIN` or
  `DELEGA_CODEX_BIN`, the configuration file, or `PATH`);
- the version, and for Claude Code whether the command hook interface that
  `--allow-command` needs is available;
- how many environment variables a worker would receive and how many would be
  withheld, as counts;
- for Claude Code, the settings files whose deny rules a run would apply;
- managed (administrator) settings found for the provider, as paths and key
  names, never values.

It also reports the configuration file (path, whether it was read, the names of
the keys it sets), the limits in force, the built-in skills, and whether process
inspection works. It never prints an environment value, a deny rule or a file's
content.

## Exit status

| Status | Meaning |
| --- | --- |
| 0 | At least one provider is ready, process inspection works and the configuration file, if any, can be used. |
| 1 | No provider is ready, the configuration file is refused, or process inspection is unavailable. |
| 2 | Doctor itself cannot run, such as from an unusable current directory. |

A provider that is not installed does not make the status 1 when the other one
is ready. To require one provider in a script, read the JSON, for example:

```sh
delega doctor --output json > doctor.json
jq -e '.providers[] | select(.provider == "claude") | .ready' doctor.json > /dev/null || exit 1
```

`providers` is a list with one entry per provider. The document's JSON Schema is
[`schemas/doctor.schema.json`](https://github.com/joshrotenberg/delega/blob/main/schemas/doctor.schema.json).

## Reading problems

Each entry in `problems` has a `kind`, a message, the provider (null for the
whole system) and what it `blocks`:

- `run`: runs for that provider (or every run) are refused or fail to start,
  for example `provider_not_found`, `settings_unreadable` or
  `managed_mcp_exclusive`.
- `command_grants`: only Claude runs that grant exact commands are refused, for
  example `unsupported_provider_version`.
- `cleanup`: runs start, but no cleanup can be confirmed
  (`process_inspection_unavailable`).
- `nothing`: a warning.

A provider is `ready` when no problem blocks `run` for it. The table of kinds is
in [Doctor](../api.md#doctor), and the managed settings locations are in
[Managed settings](../providers.md#managed-settings).

# Plugin setup

`delega plugin install` installs a plugin into Claude Code, Codex or both. The
plugin carries delega's skills and registers `delega mcp` as its MCP server, so
a host session can start and supervise workers with the MCP tools.

## Install

See what it would do first:

```sh
delega plugin install --dry-run
```

This prints the files and the host commands, and writes nothing and runs
nothing. Then install:

```sh
delega plugin install                # every host found
delega plugin install --host claude  # Claude Code only
delega plugin install --host codex   # Codex only
```

Without `--host`, each host found is installed and one not found is reported and
skipped; at least one must be found. With `--host`, every named host must be
found, or nothing is written.

It copies the running `delega` into a managed directory,
`~/.local/share/delega` unless `--install-dir` or `DELEGA_INSTALL_HOME` names
another, writes the plugin files there, and registers the plugin through the
hosts' own commands (`claude plugin ...`, `codex plugin ...`). It never edits a
host's configuration files. The files written and the host commands run are
listed under [Install and uninstall](../api.md#install-and-uninstall).

Run it again after rebuilding delega to refresh the plugin. The plugin's version
includes a digest of the copied executable, so a changed executable gets a new
version and the hosts refresh their cached copy.

`--output json` prints the command's report as JSON. The exit statuses, and the
cases with no report, are under
[Install and uninstall](../api.md#install-and-uninstall).

## Reconnect the host session

A running host session keeps the server it started with. After an install:

- In Claude Code, start a new session, or reconnect `plugin:delega:delega` from
  `/mcp`. `/reload-plugins` re-reads the skills but does not restart the
  running server.
- In Codex, start a new session.

Then read `delega://identity` and check it against
`plugin/delega/IDENTITY.json` in the managed directory.

Until the session reconnects, `list` and `delega://identity` report
`stale_server` when they detect that the server's executable was replaced or
removed. It says the file changed, not that a newer build is installed: compare
its `running_revision` with `installed_revision` when both are present
([Stale server](../api.md#stale-server)).

## Use it

The plugin gives a host session eight tools: `start`, `follow_up`, `wait`,
`read`, `list`, `interrupt`, `respond` and `close`. The
[`delegate` skill](https://github.com/joshrotenberg/delega/blob/main/crates/delega-core/skills/delegate/SKILL.md)
and the [tool table](../api.md#mcp) describe the arguments and responses.

The plugin also installs the `run` and `delegate` skills, which tell a calling
agent how to use `delega run` and the MCP tools. `delega skill list` prints them
and `delega skill show NAME` prints one.

The installed plugin starts `delega mcp` with no `--env`. Its workers get the
fixed environment allowlist plus what the configuration file's `[env]` section
adds, because `delega mcp` reads that file
([Environment policy](../providers.md#environment-policy),
[Configuration file](../api.md#configuration-file)).

Claude.ai and Cowork refuse plugins with a top-level `bin/`, so the plugin
installs only into Claude Code and Codex.

## Uninstall

```sh
delega plugin uninstall --dry-run    # print what would be removed
delega plugin uninstall              # remove from every recorded host, then the directory
delega plugin uninstall --host codex # remove from Codex, keep the directory for Claude Code
```

It removes the plugin and its marketplace from each host through the hosts' own
commands, and only when they point at the managed directory. It then deletes the
files it recorded when no host remains. It stops, keeping every file, when it
cannot confirm a registration is its own. Removing the registrations by hand,
after the managed directory was deleted, is described under
[Install and uninstall](../api.md#install-and-uninstall).

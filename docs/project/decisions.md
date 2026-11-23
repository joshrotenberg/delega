# Decisions

Design questions Josh settled, with their dates. The current behaviour is in the
[API reference](../api.md).

Settled on 2026-11-18:

| Question | Decision |
| --- | --- |
| Live permission asking | Claude Code over MCP or the Rust supervisor. |
| Default `timeout_secs` | 600 (10 minutes). |
| A run denied a permission that still finished | A recorded denial does not by itself decide the outcome. `refused` only when the run stopped because of it. |
| Names | `worker` and `run`; tools `start`, `follow_up`, `wait`, `read`, `list`, `interrupt`, `close`, and `respond`. |
| Permission modes | `read`, `write`, `full`; `build` added on 2026-11-22 ([#98](https://github.com/joshrotenberg/delega/issues/98)). |
| CLI scope | Foreground `run` only. |
| Tracing | Stderr progress only in the first version. |
| Exit status | 0 `completed`, 1 `failed`, 2 `refused`, 124 `timed_out`, 130 `interrupted`. |
| Onboard skills | Yes: one embedded catalog, reached by `delega skill`, by MCP resources through the Skills extension, and by the installed plugin. |

Settled on 2026-11-19 (issue 25, built under issue 84):

| Question | Decision |
| --- | --- |
| Where configuration lives | One optional user-level file, read by `run`, `mcp` and `doctor`. No project-level file: a cloned repository cannot change what a worker may do; profiles cover per-project setups. |
| Precedence | Built-in defaults, then the user file, then a profile, then explicit flags or tool arguments. `DELEGA_*` variables override only executables and paths. |

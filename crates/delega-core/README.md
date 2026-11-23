# delega-core

Run Claude Code and Codex agents from Rust. The crate defines a versioned
request and result contract, starts and monitors provider subprocesses, and
provides a supervisor for concurrent runs. Workers use a locally installed
and authenticated provider CLI.

See the [delega documentation](https://joshrotenberg.com/delega/) for the
contract, provider behavior and configuration.

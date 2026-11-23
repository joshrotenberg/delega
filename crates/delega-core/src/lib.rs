//! The delega library: the request and result contract and, later, the engine
//! that runs a provider as a worker.
//!
//! `docs/api.md` in the repository is the agreed design this crate follows.
//! Only the contract in [`contract`] exists so far.
//!
//! # Where each part comes from
//!
//! delega takes parts from [due](https://github.com/joshrotenberg/due) and
//! [counterpart](https://github.com/joshrotenberg/counterpart):
//!
//! | Part | due | counterpart | delega |
//! | --- | --- | --- | --- |
//! | `run` command, profiles, `--dry-run`, `--stdin` | `due run`, `--request`, `--dry-run` | none | Take due's. |
//! | Request and result as versioned JSON Schema | `run-request`, `run-result` schemas, `due schema` | Tool schemas in `contract/`, no versioning promise | Take due's shape; add `outcome`, `cleanup`, `permissions`, `provider_notes`. |
//! | Outcome set | `succeeded`, `failed`, `timed_out`, `cancelled`, `planned` | `completed`, `failed`, `interrupted`, `closed`, plus timeouts as errors | Five outcomes; `planned` becomes dry-run output. |
//! | Cleanup report | Settlement reporting | `cleanup` field, descendant identity tracking, repeated signalling | Counterpart's model: group and session sweeps, descendant tracking by start time, repeated signalling, an unconfirmed verdict with pids, groups and reason. |
//! | Permissions, stated up front | Provider options passed through, allowlisted | Tool lists, exact Bash guard hook, sandbox mode, deny rules | Provider-neutral modes over counterpart's enforcement. |
//! | Permissions, asked live | `respond`, interaction references, `Reject` mode | none | Claude Code, behind `on_request: ask`. |
//! | Long-lived provider session | Resident engine | One process per run, resumed by session id | One provider process per run, continued by session id; a resident session is deferred (issue 1). |
//! | Queue and limits | Admission limits, 16 sessions | Queue, pool of 4 running, lifetime cap of 64 | Queue with admission limits (16 live workers, 4 running, 16 queued per worker); no lifetime cap. |
//! | MCP tools | `spawn`, `follow_up`, `inspect`, `wait`, `interrupt`, `respond`, `release_run`, `release_session`, `read_items` | `spawn_agent`, `send_message`, `followup_task`, `wait_agent`, `read_agent`, `list_agents`, `interrupt_agent`, `close_agent` | `start`, `follow_up`, `wait`, `read`, `list`, `interrupt`, `respond`, `close`. One way to add work (`follow_up`, which queues) and one `close`. |
//! | Compact and full responses | none | `detail: summary\|full` with a field audit | Counterpart's idea, simpler. |
//! | Trace files and summaries | `--trace`, `--trace-file`, `due trace summarize` | Activity history in `read_agent` | Stderr progress and `read`. |
//! | Install into hosts | `due init` | `counterpart init` | `delega plugin install` and `uninstall`, through the hosts' commands. |
//! | Onboard skills | `due skills list\|show`, embedded | One catalog as `skill://counterpart/<name>/SKILL.md`, installed as a plugin, digests in the identity | One embedded catalog behind the CLI, MCP Skills extension and plugin. |
//! | Usage and cost | In the result | In the result, with scope notes | Both. |
//! | Provider executable identity | `--binary`, provenance in the result | Identity resource | Provenance in the result and `doctor`. |

pub mod canonical;

pub mod config;

pub mod contract;

pub mod doctor;

pub mod process;

pub mod profile;

pub mod provider;

pub mod skills;

pub mod supervisor;

#[cfg(test)]
mod test_spawn;

/// The version of this build, as set in the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    fn read_manifest(path: &std::path::Path) -> toml::Table {
        std::fs::read_to_string(path)
            .expect("read manifest")
            .parse()
            .expect("parse manifest")
    }

    #[test]
    fn version_matches_the_manifest() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let package = read_manifest(&root.join("Cargo.toml"));
        let version = &package["package"]["version"];
        if let Some(version) = version.as_str() {
            // Cargo writes a normalized manifest with a literal version in the archive.
            assert_eq!(super::VERSION, version);
        } else {
            assert_eq!(version["workspace"].as_bool(), Some(true));
            let workspace = read_manifest(&root.join("../../Cargo.toml"));
            assert_eq!(
                Some(super::VERSION),
                workspace["workspace"]["package"]["version"].as_str()
            );
        }
    }

    #[test]
    fn version_is_major_minor_patch_with_optional_suffix() {
        let core = super::VERSION
            .split(['-', '+'])
            .next()
            .expect("split yields one item");
        let parts: Vec<&str> = core.split('.').collect();
        assert_eq!(parts.len(), 3, "expected MAJOR.MINOR.PATCH in {core}");
        assert!(
            parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
            "non-numeric version part in {core}"
        );
    }
}

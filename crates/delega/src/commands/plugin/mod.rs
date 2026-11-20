//! What `delega plugin install` and `delega plugin uninstall` share: the managed directory, the
//! plugin payload, the ownership marker, the host commands and how a host is
//! found.
//!
//! The hosts own their configuration. Nothing here edits it: a host is
//! registered and unregistered only by running its own CLI.

mod exec;
mod files;
mod fsops;
mod install;
mod marker;
mod registration;
mod report;
mod uninstall;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Subcommand, ValueEnum};
use delega_core::contract::request::Provider;
use delega_core::provider::program::{default_name, override_variable};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Install the plugin and skills into Claude Code, Codex or both. Run it again to refresh an existing install
    Install(install::Args),
    /// Remove the delega plugin from the hosts and from disk
    Uninstall(uninstall::Args),
}

impl Command {
    pub(super) fn run(self) -> ExitCode {
        match self {
            Self::Install(args) => install::run(args),
            Self::Uninstall(args) => uninstall::run(args),
        }
    }
}

/// The local marketplace both hosts register.
pub const MARKETPLACE: &str = "delega-local";
/// The plugin name.
pub const PLUGIN: &str = "delega";
/// `plugin@marketplace`, the identifier both hosts install by.
pub const PLUGIN_ID: &str = "delega@delega-local";
/// The plugin root, relative to the managed directory.
pub const PLUGIN_DIR: &str = "plugin/delega";
/// The copied executable, relative to the managed directory.
pub const EXECUTABLE: &str = "plugin/delega/bin/delega";
/// The ownership marker, relative to the managed directory.
pub const MARKER: &str = ".delega-install.json";

/// A host `delega plugin install` can install into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Host {
    /// Claude Code
    Claude,
    /// Codex
    Codex,
}

impl Host {
    /// Both hosts, in the order they are reported.
    pub const ALL: [Host; 2] = [Host::Claude, Host::Codex];

    /// The name used on the command line, in the marker and in JSON.
    pub fn name(self) -> &'static str {
        match self {
            Host::Claude => "claude",
            Host::Codex => "codex",
        }
    }

    /// The product name, for messages.
    pub fn label(self) -> &'static str {
        match self {
            Host::Claude => "Claude Code",
            Host::Codex => "Codex",
        }
    }

    pub fn from_name(name: &str) -> Option<Host> {
        Host::ALL.into_iter().find(|host| host.name() == name)
    }

    fn provider(self) -> Provider {
        match self {
            Host::Claude => Provider::Claude,
            Host::Codex => Provider::Codex,
        }
    }

    /// The variable that names this host's executable, the same one `delega
    /// run` reads for the provider.
    pub fn variable(self) -> &'static str {
        override_variable(self.provider())
    }

    /// The host commands that register the marketplace and install the plugin.
    ///
    /// Claude Code's `plugin update` refreshes a cached copy that a repeated
    /// install leaves stale; Codex's `plugin add` copies the plugin into its
    /// cache every time, so it runs on every install.
    pub fn install_commands(self, root: &str) -> Vec<Vec<String>> {
        let args = |parts: &[&str]| parts.iter().map(|part| part.to_string()).collect();
        match self {
            Host::Claude => vec![
                args(&["plugin", "marketplace", "add", root]),
                args(&["plugin", "install", PLUGIN_ID]),
                args(&["plugin", "update", PLUGIN_ID, "--scope", "user"]),
            ],
            Host::Codex => vec![
                args(&["plugin", "marketplace", "add", root]),
                args(&["plugin", "add", PLUGIN_ID]),
            ],
        }
    }

    /// The host commands that remove the plugin and then the marketplace.
    /// Claude Code removal is limited to user scope.
    pub fn uninstall_commands(self) -> Vec<Vec<String>> {
        let args = |parts: &[&str]| parts.iter().map(|part| part.to_string()).collect();
        match self {
            Host::Claude => vec![
                args(&["plugin", "uninstall", PLUGIN_ID, "--scope", "user"]),
                args(&[
                    "plugin",
                    "marketplace",
                    "remove",
                    MARKETPLACE,
                    "--scope",
                    "user",
                ]),
            ],
            Host::Codex => vec![
                args(&["plugin", "remove", PLUGIN_ID]),
                args(&["plugin", "marketplace", "remove", MARKETPLACE]),
            ],
        }
    }

    /// What the user does so a running host picks up the plugin.
    pub fn reload(self) -> &'static str {
        match self {
            Host::Claude => {
                "start a new Claude Code session, or reconnect plugin:delega:delega from /mcp; /reload-plugins re-reads skills but does not restart the running server"
            }
            Host::Codex => "start a new Codex session",
        }
    }

    /// Whether a failed removal command is the host saying the registration
    /// was already absent. Only these exact responses are accepted; any other
    /// failure is a registration that may still exist.
    pub fn already_absent(self, output: &str) -> bool {
        let phrases: &[&str] = match self {
            Host::Claude => &[
                "not found in installed plugins",
                "Marketplace 'delega-local' not found",
            ],
            Host::Codex => &[
                "marketplace `delega-local` is not configured or installed",
                "plugin `delega` is not installed",
            ],
        };
        phrases.iter().any(|phrase| output.contains(phrase))
    }
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Find a host executable: its variable (`DELEGA_CLAUDE_BIN`,
/// `DELEGA_CODEX_BIN`, an absolute path), then its name on the absolute entries
/// of `PATH`. The error says what was looked for.
pub fn find_host(host: Host) -> Result<PathBuf, String> {
    let variable = host.variable();
    if let Some(value) = std::env::var_os(variable).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(&value);
        return if path.is_absolute() && is_executable(&path) {
            Ok(path)
        } else {
            Err(format!(
                "{variable} is {}, which is not an absolute path to an executable file",
                path.display()
            ))
        };
    }
    let name = default_name(host.provider());
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path).filter(|dir| dir.is_absolute()) {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Ok(candidate);
            }
        }
    }
    Err(format!(
        "\"{name}\" was not found on the absolute entries of PATH; set {variable} to its executable"
    ))
}

/// The managed directory: the option, then `DELEGA_INSTALL_HOME`, then
/// `$HOME/.local/share/delega`. It must be absolute. The error is a usage
/// error.
pub fn configured_dir(option: Option<PathBuf>) -> Result<PathBuf, String> {
    let non_empty = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let dir = match option {
        Some(dir) => dir,
        None => match non_empty("DELEGA_INSTALL_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(
                non_empty("HOME")
                    .ok_or("HOME is not set; pass --install-dir or set DELEGA_INSTALL_HOME")?,
            )
            .join(".local/share/delega"),
        },
    };
    if !dir.is_absolute() {
        return Err(format!(
            "the install directory must be an absolute path, got {}",
            dir.display()
        ));
    }
    Ok(dir)
}

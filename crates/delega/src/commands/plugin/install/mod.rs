//! `delega plugin install`: install a plugin that carries the delega skills and
//! registers `delega mcp` as its MCP server, into Claude Code, Codex or both.
//!
//! The plugin lives in a managed directory (default
//! `$HOME/.local/share/delega`) holding a copy of this executable, both hosts'
//! manifests, the skills and a local marketplace. Each host is then asked,
//! through its own CLI, to register that marketplace and install the plugin.
//! Nothing here edits a host's configuration, starts a server or calls a model.
//!
//! The whole host sequence runs every time. Claude Code's `plugin update`
//! refreshes its cached copy, which a repeated `plugin install` alone leaves
//! stale; Codex's `plugin add` copies the plugin into its cache each time. If a
//! command fails, the rest of that host's sequence is skipped, the other host
//! is still installed and the files stay in place. No host cache is deleted.
//! `files` and `hosts` in the marker accumulate across repeated installs.
//!
//! When the running executable is itself the managed copy, writing it is
//! skipped, so a server started from it does not see a replaced file.
//! Exit status: 0 on success and on a dry run, 1 when a host command failed or
//! something was refused, 2 for a usage error.

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use delega_core::canonical::sorted;
use delega_core::contract::SCHEMA_VERSION;
use serde_json::json;

use super::report::{CommandRecord, HostRecord};
use super::{EXECUTABLE, Host, MARKER, PLUGIN_DIR, PLUGIN_ID, exec, files, fsops, marker};
use crate::commands::{OutputFormat, output};

#[derive(Debug, Parser)]
pub struct Args {
    /// Install into this host; repeat for both. A named host must be found.
    /// Without it, each host found is installed and one not found is skipped
    #[arg(long = "host", value_enum)]
    hosts: Vec<Host>,
    /// The managed directory [default: $DELEGA_INSTALL_HOME, then
    /// $HOME/.local/share/delega]
    #[arg(long, value_name = "DIR")]
    install_dir: Option<PathBuf>,
    /// Print the files and host commands without writing or running anything
    #[arg(long)]
    dry_run: bool,
    /// Print as text or as JSON
    #[arg(long, value_enum, default_value = "text")]
    output: OutputFormat,
}

/// Everything an install needs, resolved. Tests build one with host programs
/// of their own.
pub(super) struct Request {
    /// The managed directory, with existing components resolved.
    pub dir: PathBuf,
    /// Whether the hosts came from `--host`.
    pub explicit: bool,
    /// Each wanted host with its executable, or why it was not found.
    pub hosts: Vec<(Host, Result<PathBuf, String>)>,
    /// The executable to copy.
    pub executable: PathBuf,
    pub dry_run: bool,
}

/// What an install did.
pub(super) struct Report {
    pub dry_run: bool,
    pub dir: PathBuf,
    pub plugin_version: Option<String>,
    pub executable: PathBuf,
    pub files: Vec<String>,
    pub hosts: Vec<HostRecord>,
    pub error: Option<String>,
}

impl Report {
    fn failed(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    fn json(&self) -> String {
        format!(
            "{}\n",
            sorted(json!({
                "schema_version": SCHEMA_VERSION,
                "operation": "install",
                "ok": self.error.is_none(),
                "dry_run": self.dry_run,
                "install_dir": self.dir.display().to_string(),
                "plugin_id": PLUGIN_ID,
                "plugin_version": self.plugin_version,
                "executable": self.executable.display().to_string(),
                "files": self.files,
                "hosts": self.hosts.iter().map(HostRecord::json).collect::<Vec<_>>(),
                "error": self.error,
            }))
        )
    }

    fn text(&self) -> String {
        let mut out = String::new();
        if !self.files.is_empty() {
            let verb = if self.dry_run { "Would write" } else { "Wrote" };
            out.push_str(&format!(
                "{verb} the plugin under {}:\n",
                self.dir.display()
            ));
            for file in &self.files {
                if file == EXECUTABLE {
                    out.push_str(&format!(
                        "  {file} (copy of {})\n",
                        self.executable.display()
                    ));
                } else {
                    out.push_str(&format!("  {file}\n"));
                }
            }
        }
        for host in &self.hosts {
            host.text(&mut out);
        }
        if self.error.is_none() {
            if self.dry_run {
                out.push_str("Dry run: nothing was written and no host command ran.\n");
            } else if let Some(version) = &self.plugin_version {
                out.push_str(&format!("Installed {PLUGIN_ID} {version}.\n"));
                for host in &self.hosts {
                    if let Some(reload) = host.reload {
                        out.push_str(&format!("  {}: {reload}.\n", host.host.label()));
                    }
                }
                out.push_str(&format!(
                    "Then read delega://identity in the host and compare it with {}.\n",
                    self.dir.join(PLUGIN_DIR).join("IDENTITY.json").display()
                ));
            }
        }
        out
    }
}

pub fn run(args: Args) -> ExitCode {
    let configured = match super::configured_dir(args.install_dir) {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("delega plugin install: {error}");
            return ExitCode::from(2);
        }
    };
    let explicit = !args.hosts.is_empty();
    let mut wanted = Vec::new();
    for host in if explicit {
        args.hosts
    } else {
        Host::ALL.to_vec()
    } {
        if !wanted.contains(&host) {
            wanted.push(host);
        }
    }
    let prepared = fsops::canonical_dir(&configured).and_then(|dir| {
        std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map(|executable| (dir, executable))
            .map_err(|error| format!("cannot resolve this executable: {error}"))
    });
    let report = match prepared {
        Ok((dir, executable)) => install(&Request {
            dir,
            explicit,
            hosts: wanted
                .into_iter()
                .map(|host| (host, super::find_host(host)))
                .collect(),
            executable,
            dry_run: args.dry_run,
        }),
        // Still a document, so a caller that asked for JSON always gets one.
        Err(error) => Report {
            dry_run: args.dry_run,
            dir: configured,
            plugin_version: None,
            executable: PathBuf::new(),
            files: Vec::new(),
            hosts: Vec::new(),
            error: None,
        }
        .failed(error),
    };
    let text = match args.output {
        OutputFormat::Text => report.text(),
        OutputFormat::Json => report.json(),
    };
    let wrote = output::emit("plugin install", text.as_bytes());
    match &report.error {
        None => wrote,
        Some(error) => {
            output::report(&format!("delega plugin install: {error}"));
            ExitCode::from(1)
        }
    }
}

/// Install, or with `dry_run` report what would be done.
pub(super) fn install(request: &Request) -> Report {
    let dir = &request.dir;
    let mut report = Report {
        dry_run: request.dry_run,
        dir: dir.clone(),
        plugin_version: None,
        executable: request.executable.clone(),
        files: Vec::new(),
        hosts: Vec::new(),
        error: None,
    };
    let mut found: Vec<(usize, Host, PathBuf)> = Vec::new();
    let mut missing = false;
    for (host, program) in &request.hosts {
        let mut row = HostRecord::new(*host, "skipped");
        match program {
            Ok(program) => {
                row.status = "not_run";
                row.program = Some(program.clone());
                found.push((report.hosts.len(), *host, program.clone()));
            }
            Err(reason) => {
                missing = true;
                row.note = Some(format!("not found, skipped: {reason}"));
            }
        }
        report.hosts.push(row);
    }
    if request.explicit && missing {
        return report.failed(
            "a host named with --host was not found; no file was written and no host command ran",
        );
    }
    if found.is_empty() {
        return report.failed(
            "neither Claude Code nor Codex was found; no file was written and no host command ran",
        );
    }
    let bytes = match std::fs::read(&request.executable) {
        Ok(bytes) => bytes,
        Err(error) => {
            return report.failed(format!(
                "cannot read {}: {error}",
                request.executable.display()
            ));
        }
    };
    let version = files::plugin_version(&bytes);
    report.plugin_version = Some(version.clone());
    let payload = files::payload(&version);
    let mut owned = vec![EXECUTABLE.to_string()];
    owned.extend(payload.iter().map(|(relative, _)| relative.clone()));
    report.files = owned.iter().cloned().chain([MARKER.to_string()]).collect();
    let earlier = match fsops::check_dir(dir).and_then(|()| marker::read(dir)) {
        Ok(earlier) => earlier,
        Err(error) => {
            report.files.clear();
            return report.failed(error);
        }
    };
    let previous = earlier
        .as_ref()
        .map(|marker| marker.files.clone())
        .unwrap_or_default();
    // Hosts an earlier run registered stay recorded beside this run's.
    let mut hosts = earlier
        .as_ref()
        .map(|marker| marker.hosts.clone())
        .unwrap_or_default();
    for (_, host, _) in &found {
        if !hosts.contains(host) {
            hosts.push(*host);
        }
    }
    if !request.dry_run
        && let Err(error) = write_files(
            request, &bytes, &payload, &owned, &previous, &hosts, &version,
        )
    {
        return report.failed(error);
    }
    let root = dir.to_string_lossy().into_owned();
    let mut failed = Vec::new();
    for (index, host, program) in &found {
        let row = &mut report.hosts[*index];
        let mut broke = false;
        for args in host.install_commands(&root) {
            if request.dry_run {
                row.commands.push(CommandRecord::new(args, "would_run", ""));
            } else if broke {
                row.commands.push(CommandRecord::new(args, "not_run", ""));
            } else {
                match exec::run(program, &args, exec::CHANGE_LIMIT, exec::CHANGE_BYTES) {
                    Ok(ran) if ran.success => {
                        row.commands
                            .push(CommandRecord::new(args, "ran", &ran.shown()));
                    }
                    Ok(ran) => {
                        broke = true;
                        row.commands
                            .push(CommandRecord::new(args, "failed", &ran.shown()));
                    }
                    Err(error) => {
                        broke = true;
                        row.commands
                            .push(CommandRecord::new(args, "failed", &error));
                    }
                }
            }
        }
        row.status = if request.dry_run {
            "would_install"
        } else if broke {
            failed.push(host.label());
            "failed"
        } else {
            row.reload = Some(host.reload());
            "installed"
        };
    }
    if !failed.is_empty() {
        return report.failed(format!(
            "installation did not complete for {}; the plugin files are in place, so fix the error above and run `delega plugin install` again",
            failed.join(" and ")
        ));
    }
    report
}

/// Write the marker, the executable copy, the payload and the marker again.
fn write_files(
    request: &Request,
    executable: &[u8],
    payload: &[(String, Vec<u8>)],
    owned: &[String],
    previous: &[String],
    hosts: &[Host],
    version: &str,
) -> Result<(), String> {
    let dir = &request.dir;
    let installed = dir.join(EXECUTABLE);
    let same_executable =
        std::fs::canonicalize(&installed).is_ok_and(|path| path == request.executable);
    for relative in owned.iter().chain(previous).chain([&MARKER.to_string()]) {
        fsops::refuse_symlinks(dir, relative)?;
    }
    // The marker goes first and lists earlier files too, so a failure partway
    // leaves a directory that both plugin commands still recognise.
    let mut claimed = owned.to_vec();
    claimed.extend(previous.iter().filter(|p| !owned.contains(p)).cloned());
    let existed = dir.exists();
    if let Err(error) = fsops::write(
        dir,
        MARKER,
        &marker::render(&claimed, hosts, version),
        0o644,
    ) {
        // Nothing is installed yet. A directory this run created must not be
        // left behind unmarked, where no later run would touch it.
        if !existed {
            let _ = std::fs::remove_dir(dir);
        }
        return Err(error);
    }
    if !same_executable {
        fsops::write(dir, EXECUTABLE, executable, 0o755)?;
    }
    for (relative, bytes) in payload {
        fsops::write(dir, relative, bytes, 0o644)?;
    }
    // A file an earlier version wrote and this one does not, such as a skill
    // that no longer exists, goes only after the new ones are in place.
    let stale: Vec<String> = previous
        .iter()
        .filter(|path| !owned.contains(path))
        .cloned()
        .collect();
    fsops::delete_owned(dir, &stale)?;
    fsops::write(dir, MARKER, &marker::render(owned, hosts, version), 0o644)
}

//! `delega uninstall`: remove the plugin from the hosts `init` registered it
//! with, and then the managed directory.
//!
//! Every selected host's registrations are read through its JSON listing
//! commands before any is changed, and removal proceeds only when they point
//! at this managed directory. A refusal or a failed host command keeps the
//! files. Exit status: 0 on success and on a dry run, 1 when a host command
//! failed or something was refused, 2 for a usage error.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use delega_core::canonical::sorted;
use delega_core::contract::SCHEMA_VERSION;
use serde_json::json;

use super::OutputFormat;
use super::plugin::report::{CommandRecord, HostRecord};
use super::plugin::{self, Host, MARKER, PLUGIN_ID, exec, fsops, marker, registration};

#[derive(Debug, Parser)]
pub struct Args {
    /// Remove from this host only, and keep the directory for the other;
    /// repeat for both. Without it, every host the install recorded
    #[arg(long = "host", value_enum)]
    hosts: Vec<Host>,
    /// The managed directory [default: $DELEGA_INSTALL_HOME, then
    /// $HOME/.local/share/delega]
    #[arg(long, value_name = "DIR")]
    install_dir: Option<PathBuf>,
    /// Print what would be removed without removing or running anything
    #[arg(long)]
    dry_run: bool,
    /// Print as text or as JSON
    #[arg(long, value_enum, default_value = "text")]
    output: OutputFormat,
}

/// Everything an uninstall needs, resolved.
pub(super) struct Request {
    pub dir: PathBuf,
    /// The hosts named with `--host`; empty means every recorded host.
    pub named: Vec<Host>,
    /// Each host's executable, or why it was not found.
    pub programs: Vec<(Host, Result<PathBuf, String>)>,
    pub dry_run: bool,
}

/// What an uninstall did.
pub(super) struct Report {
    pub dry_run: bool,
    pub dir: PathBuf,
    /// `removed`, `files_removed` (other files kept the directory), `kept`,
    /// `absent`, `would_remove` or `would_keep`.
    pub directory: &'static str,
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
                "operation": "uninstall",
                "ok": self.error.is_none(),
                "dry_run": self.dry_run,
                "install_dir": self.dir.display().to_string(),
                "plugin_id": PLUGIN_ID,
                "directory": self.directory,
                "files": self.files,
                "hosts": self.hosts.iter().map(HostRecord::json).collect::<Vec<_>>(),
                "error": self.error,
            }))
        )
    }

    fn text(&self) -> String {
        let mut out = String::new();
        for host in &self.hosts {
            host.text(&mut out);
        }
        let dir = self.dir.display();
        let line = match self.directory {
            "removed" => format!("Removed {dir}.\n"),
            "would_remove" => format!("Would remove {dir}.\n"),
            "files_removed" => format!(
                "Removed the files delega wrote in {dir}; other files there were left in place.\n"
            ),
            "absent" => format!("{dir} does not exist; no host command ran.\n"),
            "would_keep" => format!("Would keep {dir} for the other host.\n"),
            _ if self.error.is_none() => format!(
                "Kept {dir} for the other host; run uninstall without --host to remove it.\n"
            ),
            _ => format!("Kept {dir} and its files.\n"),
        };
        out.push_str(&line);
        if self.dry_run && self.error.is_none() {
            out.push_str("Dry run: nothing was removed and no host command ran.\n");
        }
        out
    }
}

pub fn run(args: Args) -> ExitCode {
    let configured = match plugin::configured_dir(args.install_dir) {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("delega uninstall: {error}");
            return ExitCode::from(2);
        }
    };
    let dir = match fsops::canonical_dir(&configured) {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("delega uninstall: {error}");
            return ExitCode::from(1);
        }
    };
    let mut named = Vec::new();
    for host in args.hosts {
        if !named.contains(&host) {
            named.push(host);
        }
    }
    let report = uninstall(&Request {
        dir,
        named,
        programs: Host::ALL
            .into_iter()
            .map(|host| (host, plugin::find_host(host)))
            .collect(),
        dry_run: args.dry_run,
    });
    let text = match args.output {
        OutputFormat::Text => report.text(),
        OutputFormat::Json => report.json(),
    };
    let wrote = super::output::emit("uninstall", text.as_bytes());
    match &report.error {
        None => wrote,
        Some(error) => {
            super::output::report(&format!("delega uninstall: {error}"));
            ExitCode::from(1)
        }
    }
}

fn program(request: &Request, host: Host) -> &Result<PathBuf, String> {
    &request
        .programs
        .iter()
        .find(|(h, _)| *h == host)
        .expect("a program entry for each host")
        .1
}

/// Uninstall, or with `dry_run` report what would be done.
pub(super) fn uninstall(request: &Request) -> Report {
    let dir: &Path = &request.dir;
    let mut report = Report {
        dry_run: request.dry_run,
        dir: request.dir.clone(),
        directory: "kept",
        files: Vec::new(),
        hosts: Vec::new(),
        error: None,
    };
    let installed = match marker::read(dir) {
        Ok(Some(installed)) => installed,
        Ok(None) if !dir.exists() => {
            report.directory = "absent";
            return report;
        }
        Ok(None) => {
            return report.failed(format!(
                "{} was not created by delega init; registrations and files were left in place",
                dir.display()
            ));
        }
        Err(error) => return report.failed(error),
    };
    let mut owned = installed.files.clone();
    owned.push(MARKER.to_string());
    for relative in &owned {
        if let Err(error) = fsops::refuse_symlinks(dir, relative) {
            return report.failed(error);
        }
    }
    let selected: Vec<Host> = if request.named.is_empty() {
        installed.hosts.clone()
    } else {
        request.named.clone()
    };
    // Every selected host this install registered needs its CLI, and every
    // one is verified, before any registration is changed.
    let mut programs: Vec<(Host, PathBuf)> = Vec::new();
    for host in &selected {
        if !installed.hosts.contains(host) {
            continue;
        }
        match program(request, *host) {
            Ok(path) => programs.push((*host, path.clone())),
            Err(reason) => {
                let mut row = HostRecord::new(*host, "failed");
                row.note = Some(format!("not found: {reason}"));
                report.hosts.push(row);
                return report.failed(format!(
                    "{} was registered by init but its CLI was not found, so its registration cannot be removed; the files in {} were kept",
                    host.label(),
                    dir.display()
                ));
            }
        }
    }
    let mut plans: Vec<(usize, Host, PathBuf, Vec<Vec<String>>)> = Vec::new();
    for host in selected {
        let mut row = HostRecord::new(host, "not_run");
        let Some((_, path)) = programs.iter().find(|(h, _)| *h == host) else {
            row.status = "not_registered";
            row.note = Some("this installation did not register it; nothing to remove".into());
            report.hosts.push(row);
            continue;
        };
        row.program = Some(path.clone());
        let commands = if request.dry_run {
            row.note = Some("registrations are read before removal (not run in a dry run)".into());
            host.uninstall_commands()
        } else {
            match registration::removal_commands(host, path, dir) {
                Ok(commands) => commands,
                Err(error) => {
                    row.status = "refused";
                    row.note = Some(error.clone());
                    report.hosts.push(row);
                    return report.failed(error);
                }
            }
        };
        plans.push((report.hosts.len(), host, path.clone(), commands));
        report.hosts.push(row);
    }
    let mut removed = Vec::new();
    let mut unresolved = Vec::new();
    for (index, host, path, commands) in plans {
        let row = &mut report.hosts[index];
        if commands.is_empty() {
            row.note = Some("no registration was left to remove".into());
        }
        let mut broke = false;
        for args in commands {
            if request.dry_run {
                row.commands.push(CommandRecord::new(args, "would_run", ""));
            } else if broke {
                row.commands.push(CommandRecord::new(args, "not_run", ""));
            } else {
                match exec::run(&path, &args, exec::CHANGE_LIMIT, exec::CHANGE_BYTES) {
                    Ok(ran) if ran.success => {
                        row.commands
                            .push(CommandRecord::new(args, "ran", &ran.shown()));
                    }
                    // A host that never had it exits nonzero; that is not a
                    // reason to keep the files.
                    Ok(ran) if host.already_absent(&ran.shown()) => {
                        row.commands
                            .push(CommandRecord::new(args, "already_absent", &ran.shown()));
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
            "would_remove"
        } else if broke {
            unresolved.push(host.label());
            "failed"
        } else {
            removed.push(host);
            "removed"
        };
        if request.dry_run {
            removed.push(host);
        }
    }
    let remaining: Vec<Host> = installed
        .hosts
        .iter()
        .copied()
        .filter(|host| !removed.contains(host))
        .collect();
    // Record which hosts are done, so a later run knows who still uses the files.
    if !request.dry_run
        && remaining.len() != installed.hosts.len()
        && let Err(error) = fsops::write(dir, MARKER, &installed.with_hosts(&remaining), 0o644)
    {
        return report.failed(error);
    }
    // A registration that may still exist points at these files; deleting them
    // would leave the host with a plugin it cannot start.
    if !unresolved.is_empty() {
        return report.failed(format!(
            "removal failed for {}; the files in {} were kept so the registration still works, so fix the error above and run uninstall again",
            unresolved.join(" and "),
            dir.display()
        ));
    }
    if !remaining.is_empty() {
        report.directory = if request.dry_run {
            "would_keep"
        } else {
            "kept"
        };
        return report;
    }
    report.files = owned.clone();
    if request.dry_run {
        report.directory = "would_remove";
        return report;
    }
    if let Err(error) = fsops::delete_owned(dir, &owned) {
        return report.failed(error);
    }
    // Only an empty directory is removed; anything else there is not ours.
    report.directory = match std::fs::remove_dir(dir) {
        Ok(()) => "removed",
        Err(_) => "files_removed",
    };
    report
}

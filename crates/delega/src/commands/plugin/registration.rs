//! Read a host's current registrations before removing anything, and decide
//! which removal commands are safe.
//!
//! The listings are the hosts' own JSON (`plugin marketplace list --json` and
//! `plugin list --json`), read from `/` so that neither this managed
//! directory's local marketplace nor the caller's project stands in for the
//! user's registration. Removal proceeds only when every registration under
//! the names this plugin uses points at the managed directory.

use std::path::Path;

use delega_core::process::Ending;
use serde_json::Value;

use super::exec::{self, LISTING_BYTES, LISTING_LIMIT};
use super::{Host, MARKETPLACE, PLUGIN_DIR, PLUGIN_ID};

async fn inspect(
    program: &Path,
    args: &[&str],
    runner: &mut exec::Runner,
) -> Result<Value, String> {
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let ran = runner
        .run(program, &args, LISTING_LIMIT, LISTING_BYTES)
        .await
        .map_err(|error| {
            format!("cannot verify registration: {error}; no removal was attempted")
        })?;
    // A failed listing may print plugin configuration values. Its output is
    // not echoed as a diagnostic.
    if !matches!(ran.ending, Ending::Exited { code: 0 }) {
        return Err(
            "cannot verify registration: the host listing failed; no removal was attempted".into(),
        );
    }
    if !ran.stderr.text().trim().is_empty() {
        return Err(
            "cannot verify registration: the host listing reported diagnostics; no removal was attempted"
                .into(),
        );
    }
    let value: Value = serde_json::from_str(&ran.stdout.text()).map_err(|_| {
        "cannot verify registration: unsupported host JSON response; no removal was attempted"
            .to_string()
    })?;
    if value.get("error").is_some()
        || value
            .get("errors")
            .is_some_and(|errors| errors.as_array().is_none_or(|errors| !errors.is_empty()))
    {
        return Err(
            "cannot verify registration: the host listing reported incomplete data; no removal was attempted"
                .into(),
        );
    }
    Ok(value)
}

fn same_path(value: &Value, expected: &Path) -> bool {
    value
        .as_str()
        .filter(|value| Path::new(value).is_absolute())
        .and_then(|path| std::fs::canonicalize(path).ok())
        .zip(std::fs::canonicalize(expected).ok())
        .is_some_and(|(left, right)| left == right)
}

/// The one row whose `field` is `name`. More than one is ambiguous, and a row
/// without a usable identity makes the whole listing unreadable.
fn selected<'a>(rows: &'a Value, field: &str, name: &str) -> Result<Option<&'a Value>, String> {
    let rows = rows
        .as_array()
        .ok_or("unsupported registration list shape")?;
    let mut target = None;
    for row in rows {
        let id = row[field]
            .as_str()
            .filter(|id| !id.trim().is_empty() && !id.chars().any(char::is_control))
            .ok_or("the registration list has an invalid identity")?;
        if id == name && target.replace(row).is_some() {
            return Err("the registration identity is ambiguous (several entries)".into());
        }
    }
    Ok(target)
}

/// The host commands that remove this plugin's registration, in order, or an
/// error saying why nothing may be removed. An empty list means the host has
/// no registration left to remove.
pub async fn removal_commands(
    host: Host,
    program: &Path,
    dir: &Path,
    runner: &mut exec::Runner,
) -> Result<Vec<Vec<String>>, String> {
    let marketplaces = inspect(
        program,
        &["plugin", "marketplace", "list", "--json"],
        runner,
    )
    .await?;
    let entries = match host {
        Host::Claude => &marketplaces,
        Host::Codex => &marketplaces["marketplaces"],
    };
    let marketplace = selected(entries, "name", MARKETPLACE)?;
    if let Some(entry) = marketplace {
        let points_here = match host {
            Host::Claude => {
                entry["source"] == "directory"
                    && entry.get("scope").is_none_or(|scope| scope == "user")
                    && same_path(&entry["path"], dir)
            }
            Host::Codex => same_path(&entry["root"], dir),
        };
        if !points_here {
            return Err(format!(
                "{} registers {MARKETPLACE} from another or unverified directory; no registration or file was removed",
                host.label()
            ));
        }
    }
    let plugins = inspect(program, &["plugin", "list", "--json"], runner).await?;
    let (entries, field) = match host {
        Host::Claude => (&plugins, "id"),
        Host::Codex => (&plugins["installed"], "pluginId"),
    };
    for entry in entries
        .as_array()
        .ok_or("unsupported installed-plugin list")?
    {
        if entry[field]
            .as_str()
            .is_some_and(|id| id != PLUGIN_ID && id.ends_with(&format!("@{MARKETPLACE}")))
        {
            return Err(format!(
                "{} has another plugin installed from {MARKETPLACE}; no registration or file was removed",
                host.label()
            ));
        }
    }
    let plugin = selected(entries, field, PLUGIN_ID)?;
    if let Some(entry) = plugin {
        let expected = dir.join(PLUGIN_DIR);
        let owned = marketplace.is_some()
            && match host {
                Host::Claude => {
                    entry["scope"] == "user"
                        && same_path(
                            entry.get("readFromFolder").unwrap_or(&entry["installPath"]),
                            &expected,
                        )
                }
                Host::Codex => {
                    entry["installed"] == true
                        && entry["source"]["source"] == "local"
                        && same_path(&entry["source"]["path"], &expected)
                }
            };
        if !owned {
            return Err(format!(
                "the {} plugin registration cannot be attributed to {}; reconcile it by hand, no removal was attempted",
                host.label(),
                dir.display()
            ));
        }
    }
    let mut commands = host.uninstall_commands();
    if plugin.is_none() {
        commands.remove(0);
    }
    if marketplace.is_none() {
        commands.clear();
    }
    Ok(commands)
}

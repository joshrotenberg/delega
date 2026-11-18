//! Records the source revision and the enabled Cargo features for
//! `delega version`. No dependency: it asks `git` when it can and says
//! `unknown` when it cannot.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn main() {
    let revision = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=DELEGA_GIT_REVISION={revision}");

    // `--git-path` follows a worktree's `.git` file to the real location.
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    println!("cargo:rerun-if-changed=build.rs");

    let mut features: Vec<String> = std::env::vars()
        .filter_map(|(k, _)| k.strip_prefix("CARGO_FEATURE_").map(str::to_lowercase))
        .map(|f| f.replace('_', "-"))
        .collect();
    features.sort();
    println!("cargo:rustc-env=DELEGA_FEATURES={}", features.join(","));
}

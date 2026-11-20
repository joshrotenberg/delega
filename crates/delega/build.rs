//! Records the source revision and the enabled Cargo features for
//! `delega version`. No dependency: it asks `git` when it can and says
//! `unknown` when it cannot.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    // A build script is a process of its own with no other thread spawning,
    // so there is no spawn for this one to race and no spawn lock to share
    // (#107).
    #[allow(clippy::disallowed_methods)]
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// The revision of this workspace, or `None` when the build is not inside this
/// repository. A source archive unpacked inside some other repository must not
/// report that repository's revision, so the crate has to sit at its expected
/// place below the repository root.
fn revision() -> Option<String> {
    let prefix = git(&["rev-parse", "--show-prefix"])?;
    if prefix != "crates/delega/" {
        return None;
    }
    git(&["rev-parse", "--short=12", "HEAD"])
}

fn main() {
    let revision = revision().unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=DELEGA_GIT_REVISION={revision}");

    // A commit changes the branch ref, not `HEAD`, which only names the branch.
    // Watch `HEAD` for a switch, the ref it names for a commit, and
    // `packed-refs` for a ref that was packed. `--git-path` resolves each to
    // the right directory in a linked worktree.
    let mut watched = vec!["HEAD".to_string(), "packed-refs".to_string()];
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watched.push(branch);
    }
    for name in watched {
        if let Some(path) = git(&["rev-parse", "--git-path", &name]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");

    let mut features: Vec<String> = std::env::vars()
        .filter_map(|(k, _)| k.strip_prefix("CARGO_FEATURE_").map(str::to_lowercase))
        .map(|f| f.replace('_', "-"))
        .collect();
    features.sort();
    println!("cargo:rustc-env=DELEGA_FEATURES={}", features.join(","));
}

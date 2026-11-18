//! Measures whether due-core meets delega acceptance criteria 1 and 2.
//! See README.md in the crate directory.

mod child;
mod matrix;
mod parent;
mod procs;

use std::{env, path::PathBuf, process::exit};

use matrix::{Path, Prov, Spec, lookup_case, lookup_variant};

fn main() {
    // A harness started from a background job inherits SIGINT=ignored, and an
    // ignored signal stays ignored through every exec below it. That would make
    // every fake provider ignore the SIGINT due sends, so reset it explicitly.
    for signal in [libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGHUP] {
        unsafe { libc::signal(signal, libc::SIG_DFL) };
    }
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--case") {
        // Internal: run one case in this process. See parent.rs.
        let get = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_else(|| panic!("missing {name}"))
        };
        let spec = Spec {
            path: Path::Counterpart,
            prov: if get("--prov") == "claude" {
                Prov::Claude
            } else {
                Prov::Codex
            },
            case: lookup_case(&get("--case")).expect("case"),
            variant: lookup_variant(&get("--variant")).expect("variant"),
            stream: args.iter().any(|a| a == "--stream"),
        };
        let dir = PathBuf::from(env::var("SPIKE_DIR").expect("SPIKE_DIR"));
        child::run(spec, dir);
    }
    exit(parent::main(&args));
}

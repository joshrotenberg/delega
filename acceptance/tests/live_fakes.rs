//! The live suite's plumbing against the fake providers: building the
//! arguments, starting `delega`, reading its result or doctor report, judging
//! it and removing the case directory. A temporary `HOME`, a cleared
//! environment and the fakes as the provider executables, so no real
//! provider, login or network is used. The live cases themselves run only with
//! `delega-acceptance --live`.

use std::ffi::OsString;
use std::path::Path;

use delega_acceptance::delega_bin;
use delega_acceptance::live::{self, Case, Env, Live, Planned};

fn fakes() -> std::path::PathBuf {
    delega_bin::workspace_root().join("crates/delega-core/src/provider/tests/fakes")
}

fn suite(scratch: &Path) -> Live {
    let delega = delega_bin::build(false).expect("build delega");
    let home = scratch.join("home");
    let config = home.join("config");
    let root = scratch.join("root");
    for d in [&home, &config, &root] {
        std::fs::create_dir_all(d).expect("create a directory");
    }
    let vars: Vec<(OsString, OsString)> = [
        ("PATH", OsString::from("/usr/bin:/bin")),
        ("HOME", home.clone().into()),
        ("XDG_CONFIG_HOME", config.clone().into()),
        ("CLAUDE_CONFIG_DIR", config.clone().into()),
        ("CODEX_HOME", config.into()),
        ("DELEGA_CLAUDE_BIN", fakes().join("fake-claude.sh").into()),
        ("DELEGA_CODEX_BIN", fakes().join("fake-codex.sh").into()),
        ("DELEGA_TEST_SYSTEM_ROOT", scratch.join("system").into()),
    ]
    .into_iter()
    .map(|(k, v)| (OsString::from(k), v))
    .collect();
    let root = std::fs::canonicalize(root).expect("canonical root");
    Live::new(delega, root, Env::Fixed(vars))
}

#[test]
fn the_plain_and_doctor_cases_pass_against_the_fakes_and_leave_no_case_directory() {
    let scratch = tempfile::tempdir().expect("scratch");
    let suite = suite(scratch.path());
    for provider in live::PROVIDERS {
        for case in [Case::Plain, Case::Doctor] {
            let row = suite.run(Planned { provider, case });
            assert_eq!(
                row.verdict,
                "pass",
                "{provider} {}: {:?}",
                case.name(),
                row.problems
            );
            assert!(row.provider_version.is_some(), "{row:?}");
        }
    }
    let left: Vec<_> = std::fs::read_dir(&suite.root)
        .expect("read the root")
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(left.is_empty(), "case directories left: {left:?}");
}

#[test]
fn a_case_whose_answer_misses_the_seeded_word_fails_rather_than_passing_vacuously() {
    let scratch = tempfile::tempdir().expect("scratch");
    let suite = suite(scratch.path());
    // The fake answers "fake final answer", which has no seeded word.
    let row = suite.run(Planned {
        provider: "claude",
        case: Case::ReadCwd,
    });
    assert_eq!(row.verdict, "fail", "{row:?}");
    assert_eq!(row.outcome.as_deref(), Some("completed"));
    assert_eq!(row.cleanup_confirmed, Some(true));
    assert_eq!(
        row.problems,
        vec!["not so: the answer has the word seeded in the working directory"]
    );
}

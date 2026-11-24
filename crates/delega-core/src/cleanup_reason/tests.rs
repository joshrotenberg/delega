//! Each producer of an `Unconfirmed` reason against the recognisers: a reason
//! without a recogniser fails here, not in an acceptance row.

use std::io;
use std::path::PathBuf;

use super::*;
use crate::process::not_started;

/// The OS error numbers used here (ENOENT 2, EINTR 4, EACCES 13, EPIPE 32)
/// are the same on Linux and macOS.
///
/// One error from every constructor of a process listing error, in
/// `process/tree.rs` and `process/marker.rs`.
fn every_listing_error() -> Vec<String> {
    let mut all: Vec<String> = ListingFailure::ALL
        .iter()
        .map(|f| f.text().to_string())
        .collect();
    all.push(ps_io_error(&io::Error::from_raw_os_error(32)));
    all.push(ps_io_error(&io::Error::from_raw_os_error(4)));
    all.push(ps_exit(Some(1)));
    all.push(ps_exit(Some(-1)));
    all.push(ps_exit(None));
    all.push(unparsed_lines(1));
    all.push(unparsed_lines(42));
    all.push(not_started(&[]).to_string());
    let enoent = || io::Error::from_raw_os_error(2);
    let eacces = || io::Error::from_raw_os_error(13);
    all.push(not_started(&[(PathBuf::from(PS_PROGRAMS[0]), enoent())]).to_string());
    all.push(
        not_started(&[
            (PathBuf::from(PS_PROGRAMS[0]), eacces()),
            (PathBuf::from(PS_PROGRAMS[1]), enoent()),
        ])
        .to_string(),
    );
    // Cut at the length limit.
    let many: Vec<_> = (0..40)
        .map(|_| (PathBuf::from(PS_PROGRAMS[1]), enoent()))
        .collect();
    let cut = not_started(&many).to_string();
    assert!(cut.ends_with(NOT_STARTED_CUT), "{cut}");
    all.push(cut);
    all
}

/// One reason from every producer of a process report's reason, with every
/// value of each closed set.
fn every_process_reason() -> Vec<String> {
    let mut all: Vec<String> = FIXED.iter().map(|r| (*r).to_string()).collect();
    all.push(tree_left_at_reap(1));
    all.push(tree_left_at_reap(12));
    for error in every_listing_error() {
        for checked in GroupCheck::ALL {
            for listed in [false, true] {
                all.push(inspection_unavailable(&error, *checked, listed));
            }
        }
    }
    for waited in [Duration::ZERO, Duration::from_millis(2_345), Duration::MAX] {
        for detail in StopDetail::ALL {
            all.push(stopping_unconfirmed(waited, Ok(*detail)));
        }
        for error in every_listing_error() {
            all.push(stopping_unconfirmed(waited, Err(&error)));
        }
    }
    for cause in OwnerLost::ALL {
        for kept in [false, true] {
            all.push(owner_lost(*cause, kept));
        }
    }
    all
}

fn every_standalone_reason() -> Vec<String> {
    RunLost::ALL.iter().map(|c| run_lost(*c)).collect()
}

#[test]
fn every_fixed_reason_is_recognised() {
    for reason in FIXED {
        assert!(is_documented(reason), "{reason}");
    }
}

#[test]
fn every_reason_the_process_owner_builds_is_recognised() {
    // owner.rs: the count left at the reap, the inspection-unavailable verdict
    // and the stop that was not confirmed in time, each with every listing
    // error tree.rs and marker.rs build.
    for count in [1, 7, usize::MAX] {
        assert!(is_documented(&tree_left_at_reap(count)));
    }
    for error in every_listing_error() {
        for checked in GroupCheck::ALL {
            for listed in [false, true] {
                let reason = inspection_unavailable(&error, *checked, listed);
                assert!(is_documented(&reason), "{reason}");
            }
        }
    }
    for waited in [
        Duration::ZERO,
        Duration::from_secs(5),
        Duration::from_millis(12_345),
        Duration::MAX,
    ] {
        for detail in StopDetail::ALL {
            let reason = stopping_unconfirmed(waited, Ok(*detail));
            assert!(is_documented(&reason), "{reason}");
        }
        for error in every_listing_error() {
            let reason = stopping_unconfirmed(waited, Err(&error));
            assert!(is_documented(&reason), "{reason}");
        }
    }
}

#[test]
fn every_reason_the_process_handle_builds_is_recognised() {
    for cause in OwnerLost::ALL {
        for kept in [false, true] {
            let reason = owner_lost(*cause, kept);
            assert!(is_documented(&reason), "{reason}");
        }
    }
}

#[test]
fn every_reason_the_provider_run_and_the_supervisor_build_is_recognised() {
    for cause in RunLost::ALL {
        let reason = run_lost(*cause);
        assert!(is_documented(&reason), "{reason}");
    }
}

#[test]
fn every_recogniser_recognises_a_produced_reason() {
    let process = every_process_reason();
    for (i, recognise) in PROCESS_RECOGNISERS.iter().enumerate() {
        assert!(
            process.iter().any(|r| recognise(r)),
            "process recogniser {i} recognises no produced reason"
        );
    }
    let standalone = every_standalone_reason();
    for (i, recognise) in STANDALONE_RECOGNISERS.iter().enumerate() {
        assert!(
            standalone.iter().any(|r| recognise(r)),
            "standalone recogniser {i} recognises no produced reason"
        );
    }
}

#[test]
fn up_to_three_process_reasons_joined_by_merge_cleanup_are_recognised_in_any_order() {
    let all = every_process_reason();
    for first in &all {
        for second in &all {
            let joined = format!("{first}; {second}");
            assert!(is_documented(&joined), "{joined}");
        }
    }
    let samples = [
        LATE_LISTING_REASON.to_string(),
        owner_lost(OwnerLost::Panicked, true),
        stopping_unconfirmed(
            Duration::from_secs(1),
            Err(&ps_io_error(&io::Error::from_raw_os_error(32))),
        ),
        inspection_unavailable(&not_started(&[]).to_string(), GroupCheck::Present, true),
    ];
    for a in &samples {
        for b in &samples {
            for c in &samples {
                let joined = format!("{a}; {b}; {c}");
                assert!(is_documented(&joined), "{joined}");
            }
        }
    }
}

#[test]
fn more_reasons_than_a_provider_run_merges_are_not_recognised() {
    let late = LATE_LISTING_REASON;
    assert!(is_documented(&format!("{late}; {late}; {late}")));
    assert!(!is_documented(&format!("{late}; {late}; {late}; {late}")));
    let pipes = PIPES_HELD_REASON;
    assert!(!is_documented(&format!(
        "{late}; {pipes}; {}; {late}",
        owner_lost(OwnerLost::ReportTaken, false)
    )));
}

#[test]
fn a_run_loss_reason_joined_to_any_other_is_not_recognised() {
    for lost in every_standalone_reason() {
        for other in every_process_reason()
            .into_iter()
            .chain(every_standalone_reason())
        {
            for joined in [format!("{lost}; {other}"), format!("{other}; {lost}")] {
                assert!(!is_documented(&joined), "{joined:?}");
            }
        }
    }
}

#[test]
fn a_near_miss_of_a_produced_reason_is_not_recognised() {
    for reason in every_process_reason()
        .into_iter()
        .chain(every_standalone_reason())
    {
        let mut truncated = reason.clone();
        truncated.pop();
        for near in [
            format!("{reason}."),
            format!("{reason} and more"),
            format!("{reason};"),
            format!("{reason}; "),
            format!("{reason}; unknown reason"),
            format!("unknown reason; {reason}"),
            format!(" {reason}"),
            format!("x{reason}"),
            truncated,
            reason.to_uppercase(),
        ] {
            assert!(!is_documented(&near), "{near:?}");
        }
    }
    for near in [
        String::new(),
        "could not be checked".to_string(),
        format!("0 {TREE_LEFT_AT_REAP_REASON}"),
        format!("03 {TREE_LEFT_AT_REAP_REASON}"),
        format!("+3 {TREE_LEFT_AT_REAP_REASON}"),
        format!("-3 {TREE_LEFT_AT_REAP_REASON}"),
        format!("three {TREE_LEFT_AT_REAP_REASON}"),
        TREE_LEFT_AT_REAP_REASON.to_string(),
        format!("{STOPPING_START}2 s: {}", StopDetail::NotClean.text()),
        format!("{STOPPING_START}2.35 s: {}", StopDetail::NotClean.text()),
        format!("{STOPPING_START}NaN s: {}", StopDetail::NotClean.text()),
        format!("{STOPPING_START}: {}", StopDetail::NotClean.text()),
        format!("{STOPPING_START}2.0 s: something else"),
        format!("something else{OWNER_LOST_KILL}"),
        OWNER_LOST_KILL.to_string(),
        format!("something else{RUN_LOST_END}"),
        RUN_LOST_END.to_string(),
    ] {
        assert!(!is_documented(&near), "{near:?}");
    }
}

#[test]
fn a_wait_beyond_the_longest_duration_is_not_recognised() {
    let detail = StopDetail::NotClean.text();
    assert!(is_documented(&stopping_unconfirmed(
        Duration::MAX,
        Ok(StopDetail::NotClean)
    )));
    for waited in ["100000000000000000000.0", "18446744073709600000.0", "1e30"] {
        let reason = format!("{STOPPING_START}{waited} s: {detail}");
        assert!(!is_documented(&reason), "{reason}");
    }
}

#[test]
fn a_listing_error_outside_the_forms_tree_and_marker_build_is_not_recognised() {
    let os = |text: &str| format!("{text} (os error 2)");
    let long_cut = {
        let mut s = format!("{NOT_STARTED_START}{}: ", PS_PROGRAMS[0]);
        while s.chars().count() < MAX_ERROR_CHARS - NOT_STARTED_CUT.len() {
            s.push('x');
        }
        s
    };
    let errors = [
        String::new(),
        "anything".to_string(),
        "EPERM".to_string(),
        format!("{}; unknown reason", ListingFailure::TimedOut.text()),
        format!("{} and more", ListingFailure::TimedOut.text()),
        "ps: Broken pipe".to_string(),
        format!("ps: {}", os("")),
        format!("ps: {}", os("Broken pipe; unknown reason")),
        format!("ps: {}", os("Broken\npipe")),
        "ps: Broken pipe (os error x)".to_string(),
        "ps: Broken pipe (os error 02)".to_string(),
        format!("ps: {}; unknown reason", os("Broken pipe")),
        "ps failed (exit 1)".to_string(),
        "ps failed (exit Some(01))".to_string(),
        "ps failed (exit Some())".to_string(),
        "ps failed (exit Some(1)); unknown reason".to_string(),
        "the process listing had 0 lines that could not be parsed".to_string(),
        "the process listing had many lines that could not be parsed".to_string(),
        format!("{NOT_STARTED_START})"),
        format!("{NOT_STARTED_START}{NO_PS_PROGRAM}; unknown reason)"),
        format!("{NOT_STARTED_START}/opt/ps: {})", os("No such file")),
        format!("{NOT_STARTED_START}{}: unknown)", PS_PROGRAMS[0]),
        format!(
            "{NOT_STARTED_START}{}: {}, unknown)",
            PS_PROGRAMS[0],
            os("No such file")
        ),
        // Over the limit, or cut but shorter than it, or cut text with `;`.
        format!("{long_cut}x{NOT_STARTED_CUT}"),
        format!("{}{NOT_STARTED_CUT}", &long_cut[..long_cut.len() - 1]),
        format!("{};{NOT_STARTED_CUT}", &long_cut[..long_cut.len() - 1]),
    ];
    for error in errors {
        assert!(!is_listing_error(&error), "{error:?}");
        let stopping = format!("{STOPPING_START}1.0 s: {INSPECTION_FAILED}{error}");
        assert!(!is_documented(&stopping), "{stopping:?}");
        for checked in GroupCheck::ALL {
            let unavailable = inspection_unavailable(&error, *checked, false);
            assert!(!is_documented(&unavailable), "{unavailable:?}");
        }
    }
    // The cut form at exactly the limit is one tree.rs can build.
    let at_limit = format!("{long_cut}{NOT_STARTED_CUT}");
    assert_eq!(at_limit.chars().count(), MAX_ERROR_CHARS);
    assert!(is_listing_error(&at_limit));
}

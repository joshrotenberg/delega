//! Each producer of an `Unconfirmed` reason against the recognisers: a reason
//! without a recogniser fails here, not in an acceptance row.

use super::*;

/// One reason from every producer, with every value of each closed set.
fn every_produced_reason() -> Vec<String> {
    let mut all: Vec<String> = FIXED.iter().map(|r| (*r).to_string()).collect();
    all.push(tree_left_at_reap(1));
    all.push(tree_left_at_reap(12));
    for checked in GroupCheck::ALL {
        for listed in [false, true] {
            all.push(inspection_unavailable("ps failed", *checked, listed));
        }
    }
    for waited in [Duration::ZERO, Duration::from_millis(2_345)] {
        for detail in StopDetail::ALL {
            all.push(stopping_unconfirmed(waited, Ok(*detail)));
        }
        all.push(stopping_unconfirmed(waited, Err("ps failed")));
    }
    for cause in OwnerLost::ALL {
        for kept in [false, true] {
            all.push(owner_lost(*cause, kept));
        }
    }
    for cause in RunLost::ALL {
        all.push(run_lost(*cause));
    }
    all
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
    // and the stop that was not confirmed in time.
    for count in [1, 7, usize::MAX] {
        assert!(is_documented(&tree_left_at_reap(count)));
    }
    for checked in GroupCheck::ALL {
        for listed in [false, true] {
            let reason = inspection_unavailable("No such file (os error 2)", *checked, listed);
            assert!(is_documented(&reason), "{reason}");
        }
    }
    for waited in [
        Duration::ZERO,
        Duration::from_secs(5),
        Duration::from_millis(12_345),
    ] {
        for detail in StopDetail::ALL {
            let reason = stopping_unconfirmed(waited, Ok(*detail));
            assert!(is_documented(&reason), "{reason}");
        }
        let reason = stopping_unconfirmed(waited, Err("ps exited 1: a; b: c"));
        assert!(is_documented(&reason), "{reason}");
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
    let all = every_produced_reason();
    for (i, recognise) in RECOGNISERS.iter().enumerate() {
        assert!(
            all.iter().any(|r| recognise(r)),
            "recogniser {i} recognises no produced reason"
        );
    }
}

#[test]
fn reasons_joined_by_merge_cleanup_are_recognised_in_any_order() {
    let all = every_produced_reason();
    for first in &all {
        for second in &all {
            let joined = format!("{first}; {second}");
            assert!(is_documented(&joined), "{joined}");
        }
    }
    let three = format!(
        "{LATE_LISTING_REASON}; {}; {LATE_LISTING_REASON}",
        stopping_unconfirmed(Duration::from_secs(1), Err("listing failed; retried"))
    );
    assert!(is_documented(&three), "{three}");
}

#[test]
fn a_near_miss_of_a_produced_reason_is_not_recognised() {
    for reason in every_produced_reason() {
        let mut near = vec![
            format!("unknown reason; {reason}"),
            format!(" {reason}"),
            format!("x{reason}"),
            reason.to_uppercase(),
        ];
        // A failed listing's error is free text at the end of its reason, so
        // text after it, or a shorter error, is still that reason.
        if !reason.contains(INSPECTION_FAILED) {
            let mut truncated = reason.clone();
            truncated.pop();
            near.extend([
                format!("{reason}."),
                format!("{reason} and more"),
                format!("{reason};"),
                format!("{reason}; "),
                format!("{reason}; unknown reason"),
                truncated,
            ]);
        }
        for near in near {
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
        inspection_unavailable("", GroupCheck::Empty, false),
        inspection_unavailable("e", GroupCheck::Empty, false)
            .replace(GroupCheck::Empty.text(), "nothing was checked"),
        format!("{STOPPING_START}2 s: {}", StopDetail::NotClean.text()),
        format!("{STOPPING_START}2.35 s: {}", StopDetail::NotClean.text()),
        format!("{STOPPING_START}NaN s: {}", StopDetail::NotClean.text()),
        format!("{STOPPING_START}: {}", StopDetail::NotClean.text()),
        format!("{STOPPING_START}2.0 s: something else"),
        stopping_unconfirmed(Duration::from_secs(2), Err("")),
        format!("something else{OWNER_LOST_KILL}"),
        OWNER_LOST_KILL.to_string(),
        format!("something else{RUN_LOST_END}"),
        RUN_LOST_END.to_string(),
    ] {
        assert!(!is_documented(&near), "{near:?}");
    }
}

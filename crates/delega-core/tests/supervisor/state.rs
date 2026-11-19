//! The state tables: every pair of states, and the illegal ones.

use delega_core::supervisor::{IllegalTransition, RunState, WorkerState};

const RUN_STATES: [RunState; 3] = [RunState::Queued, RunState::Running, RunState::Finished];
const WORKER_STATES: [WorkerState; 4] = [
    WorkerState::Queued,
    WorkerState::Running,
    WorkerState::Idle,
    WorkerState::Closed,
];

#[test]
fn a_run_may_only_move_forward_and_finished_is_final() {
    use RunState::*;
    let allowed = [(Queued, Running), (Queued, Finished), (Running, Finished)];
    for from in RUN_STATES {
        for to in RUN_STATES {
            let expected = allowed.contains(&(from, to));
            assert_eq!(from.can_become(to), expected, "{from:?} -> {to:?}");
            match from.transition(to) {
                Ok(state) => assert!(expected && state == to),
                Err(error) => assert_eq!(error, IllegalTransition { from, to }),
            }
        }
    }
}

#[test]
fn a_worker_follows_the_documented_table_and_closed_is_final() {
    use WorkerState::*;
    let allowed = [
        (Queued, Running),
        (Queued, Idle),
        (Queued, Closed),
        (Running, Queued),
        (Running, Idle),
        (Running, Closed),
        (Idle, Queued),
        (Idle, Running),
        (Idle, Closed),
    ];
    for from in WORKER_STATES {
        for to in WORKER_STATES {
            let expected = allowed.contains(&(from, to));
            assert_eq!(from.can_become(to), expected, "{from:?} -> {to:?}");
            assert_eq!(from.transition(to).is_ok(), expected);
        }
    }
    for to in WORKER_STATES {
        assert!(!Closed.can_become(to), "closed must be final");
    }
}

#[test]
fn an_illegal_transition_names_both_states() {
    let error = RunState::Finished
        .transition(RunState::Running)
        .expect_err("finished is final");
    assert_eq!(
        error.to_string(),
        "illegal transition from Finished to Running"
    );
}

#[test]
fn a_worker_state_follows_from_what_it_holds() {
    assert_eq!(WorkerState::derive(true, true, 3), WorkerState::Closed);
    assert_eq!(WorkerState::derive(false, true, 3), WorkerState::Running);
    assert_eq!(WorkerState::derive(false, false, 1), WorkerState::Queued);
    assert_eq!(WorkerState::derive(false, false, 0), WorkerState::Idle);
}

//! Fifty runs across ten workers, end to end.

use std::collections::{HashMap, HashSet};

use super::{Rig, read_full};
use crate::contract::outcome::Outcome;
use crate::contract::request::Provider;
use crate::supervisor::{Limits, ResultView, Supervisor, WaitTargets, WorkerState};

#[tokio::test]
async fn fifty_runs_across_ten_workers_all_complete_in_order_and_resume_their_sessions() {
    let rig = Rig::new();
    let limits = Limits {
        max_live_workers: 10,
        max_running_runs: 4,
        ..Limits::default()
    };
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, limits));

    // Each worker gets its first run and four follow-ups, queued at once.
    let mut by_worker: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for w in 0..10 {
        let first = supervisor
            .start(rig.request(Provider::Claude, &format!("echo:w{w}-r0")))
            .await
            .expect("start");
        let worker = first.worker.clone().expect("worker");
        let mut runs = vec![(first.run, format!("echo:w{w}-r0"))];
        for r in 1..5 {
            let prompt = format!("echo:w{w}-r{r}");
            let queued = supervisor
                .follow_up(&worker, &prompt, None)
                .await
                .expect("follow_up");
            assert!(queued.outcome.is_none(), "{queued:?}");
            runs.push((queued.run, prompt));
        }
        by_worker.insert(worker, runs);
    }
    let all: Vec<String> = by_worker
        .values()
        .flatten()
        .map(|(run, _)| run.clone())
        .collect();
    assert_eq!(all.len(), 50);
    assert_eq!(
        all.iter().collect::<HashSet<_>>().len(),
        50,
        "ids are unique"
    );

    let list = supervisor.list().await.expect("list");
    assert!(list.limits.running_runs <= 4, "{:?}", list.limits);

    // Collect every finish through the cursor; no finish is missed or doubled.
    let mut seen = HashSet::new();
    let mut cursor = None;
    let mut peak_running = 0;
    while seen.len() < all.len() {
        let response = supervisor
            .wait(
                WaitTargets {
                    runs: all.clone(),
                    workers: Vec::new(),
                },
                Some(60),
                cursor,
            )
            .await
            .expect("wait");
        assert!(!response.timed_out, "{} of 50 finished", seen.len());
        for run in &response.runs {
            let new = match cursor {
                None => run.outcome.is_some(),
                Some(_) => run.changed == Some(true),
            };
            if new {
                assert!(seen.insert(run.run.clone()), "{} reported twice", run.run);
            }
        }
        cursor = Some(response.cursor);
        peak_running = peak_running.max(supervisor.list().await.expect("list").limits.running_runs);
        assert!(peak_running <= 4);
    }

    for (worker, runs) in &by_worker {
        let mut finished_at = Vec::new();
        for (run, prompt) in runs {
            let read = read_full(&supervisor, run).await;
            let Some(ResultView::Full(result)) = read.result else {
                panic!("run {run} did not finish");
            };
            assert_eq!(result.validate(), Ok(()));
            assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
            assert_eq!(result.worker.as_ref().map(|w| &w.id), Some(worker));
            assert_eq!(
                result.text,
                format!("answered {prompt} sid={}", rig.session())
            );
            assert_eq!(result.cleanup, crate::contract::result::Cleanup::Confirmed);
            finished_at.push(result.timing.finished_at.clone());
        }
        let mut sorted = finished_at.clone();
        sorted.sort();
        assert_eq!(
            finished_at, sorted,
            "runs of {worker} finished in the order given"
        );
    }
    let list = supervisor.list().await.expect("list");
    assert_eq!(list.limits.running_runs, 0);
    assert!(
        list.workers
            .iter()
            .all(|w| w.brief.state == WorkerState::Idle)
    );
    assert!(list.workers.iter().all(|w| w.brief.queue_length == 0));

    let report = supervisor.shutdown().await;
    assert_eq!(report.workers_closed, 10);
    assert!(report.unsettled.is_empty());
    rig.assert_nothing_left().await;
}

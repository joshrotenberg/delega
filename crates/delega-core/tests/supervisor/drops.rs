//! Dropping the last clone of a supervisor, wherever that happens.
//!
//! A drop signals every provider and returns; it never waits and never panics.
//! The processes are gone shortly after. Each test drops with ten workers
//! running and checks three things: the drop returns promptly (far inside the
//! 30 s shutdown bound it must not wait for), it does not panic, and no
//! provider process is left once the test has polled for the state to settle.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::stops::{start_ten, ten_workers_limits};
use super::{Rig, Supervisor};
use delega_core::contract::request::Provider;

/// How long a drop may take. It does no waiting, so this is generous.
const PROMPTLY: Duration = Duration::from_secs(5);

fn ten(rig: &Rig) -> Supervisor {
    Supervisor::new(rig.config_with(Provider::Claude, ten_workers_limits()))
}

fn timed_drop(supervisor: Supervisor) -> Duration {
    let begun = Instant::now();
    drop(supervisor);
    begun.elapsed()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inside_a_spawned_task_with_several_runtime_threads() {
    let rig = Rig::new();
    let supervisor = ten(&rig);
    start_ten(&rig, &supervisor).await;
    assert!(!rig.survivors().is_empty());
    let took = tokio::spawn(async move { timed_drop(supervisor) })
        .await
        .expect("the task that drops the supervisor");
    assert!(took < PROMPTLY, "the drop took {took:?}");
    rig.assert_nothing_left().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn inside_a_spawned_task_with_one_runtime_thread() {
    let rig = Rig::new();
    let supervisor = ten(&rig);
    start_ten(&rig, &supervisor).await;
    let took = tokio::spawn(async move { timed_drop(supervisor) })
        .await
        .expect("the task that drops the supervisor");
    assert!(took < PROMPTLY, "the drop took {took:?}");
    rig.assert_nothing_left().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_the_thread_that_called_block_on() {
    let rig = Rig::new();
    let supervisor = ten(&rig);
    start_ten(&rig, &supervisor).await;
    let took = timed_drop(supervisor);
    assert!(took < PROMPTLY, "the drop took {took:?}");
    rig.assert_nothing_left().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inside_a_local_set() {
    let rig = Rig::new();
    let supervisor = ten(&rig);
    start_ten(&rig, &supervisor).await;
    let local = tokio::task::LocalSet::new();
    let took = local
        .run_until(async move {
            tokio::task::spawn_local(async move { timed_drop(supervisor) })
                .await
                .expect("the local task that drops the supervisor")
        })
        .await;
    assert!(took < PROMPTLY, "the drop took {took:?}");
    rig.assert_nothing_left().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn while_unwinding_from_a_panic() {
    let rig = Rig::new();
    let supervisor = ten(&rig);
    start_ten(&rig, &supervisor).await;
    let begun = Instant::now();
    // The supervisor is dropped by the unwinding itself. A drop that panicked
    // here would abort the process, so reaching the next line is the test.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _held = supervisor;
        panic!("unwinding with a supervisor held");
    }));
    assert!(outcome.is_err());
    assert!(begun.elapsed() < PROMPTLY, "{:?}", begun.elapsed());
    rig.assert_nothing_left().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inside_spawn_blocking() {
    let rig = Rig::new();
    let supervisor = ten(&rig);
    start_ten(&rig, &supervisor).await;
    assert!(!rig.survivors().is_empty());
    // The last handle is dropped on a blocking-pool thread of the runtime.
    let took = tokio::task::spawn_blocking(move || timed_drop(supervisor))
        .await
        .expect("the blocking task that drops the supervisor did not panic");
    assert!(took < Duration::from_secs(60), "the drop took {took:?}");
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn on_a_current_thread_runtime() {
    let rig = Rig::new();
    let supervisor = ten(&rig);
    start_ten(&rig, &supervisor).await;
    let took = timed_drop(supervisor);
    // Every provider has been signalled; this runtime stops them once polled.
    assert!(took < PROMPTLY, "the drop took {took:?}");
    rig.assert_nothing_left().await;
}

#[test]
fn from_a_plain_thread_with_no_runtime() {
    let rig = Arc::new(Rig::new());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build a runtime");
    let supervisor = runtime.block_on({
        let rig = rig.clone();
        async move {
            let supervisor = ten(&rig);
            start_ten(&rig, &supervisor).await;
            supervisor
        }
    });
    // A thread that is not in any runtime context, so there is no handle.
    let took = std::thread::spawn(move || timed_drop(supervisor))
        .join()
        .expect("the plain thread did not panic");
    assert!(took < PROMPTLY, "the drop took {took:?}");
    rig.settle_blocking();
    drop(runtime);
}

#[test]
fn when_the_runtime_is_dropped_while_a_task_holds_the_last_clone() {
    let rig = Arc::new(Rig::new());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build a runtime");
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    runtime.spawn({
        let rig = rig.clone();
        async move {
            let supervisor = ten(&rig);
            start_ten(&rig, &supervisor).await;
            ready_tx.send(()).expect("tell the test the workers run");
            // Hold the last clone until the runtime drops this task.
            std::future::pending::<()>().await;
            drop(supervisor);
        }
    });
    ready_rx
        .recv_timeout(Duration::from_secs(120))
        .expect("the ten workers started");
    assert!(!rig.survivors().is_empty());

    let begun = Instant::now();
    drop(runtime);
    assert!(
        begun.elapsed() < Duration::from_secs(10),
        "the runtime drop took {:?}",
        begun.elapsed()
    );
    // The process layer's drop path kills each child's group without a listing.
    rig.settle_blocking();
}

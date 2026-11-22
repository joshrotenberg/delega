//! A bound that ends at its limit on either clock.
//!
//! Tokio's timers count the time the machine was awake: `Instant` is
//! `CLOCK_UPTIME_RAW` on macOS and `CLOCK_MONOTONIC` on Linux, and neither
//! advances while the machine sleeps. A caller of `wait`, `interrupt` or
//! `close` measures the call in wall time, so a bound on awake time alone can
//! outlast what the caller allows by however long the machine slept.
//! [`within`] ends when the limit has passed on either clock.
//!
//! Run time limits are not bounded this way on purpose: a run paused by sleep
//! has not used its time.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::time::{Instant, MissedTickBehavior};

use super::limits::WALL_CHECK_INTERVAL;

/// Where [`within`] reads wall time. `SystemTime::now` outside tests.
pub(super) type WallClock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// The system's wall clock.
pub(super) fn system_clock() -> WallClock {
    Arc::new(SystemTime::now)
}

/// Await `future` for at most `limit`, counted in awake time and in wall time,
/// whichever passes first. `None` means the limit passed.
///
/// The awake-time bound is a timer, exact as before. The wall-time bound is
/// checked every [`WALL_CHECK_INTERVAL`] of awake time, so after a sleep it ends
/// within that interval of the machine waking. A wall clock that reads earlier
/// than it did at the start counts as no time passed, which leaves the
/// awake-time bound in force; one set forward ends the bound early.
///
/// `future` is polled first, so one that is ready is returned even when `limit`
/// is zero. It is taken by value: pass `&mut` to keep it after the limit.
pub(super) async fn within<F: Future>(
    limit: Duration,
    clock: &WallClock,
    future: F,
) -> Option<F::Output> {
    let began = clock();
    let awake = tokio::time::sleep(limit);
    let mut check =
        tokio::time::interval_at(Instant::now() + WALL_CHECK_INTERVAL, WALL_CHECK_INTERVAL);
    // After a sleep the checks do not need to catch up: one is enough.
    check.set_missed_tick_behavior(MissedTickBehavior::Delay);
    tokio::pin!(future, awake);
    loop {
        tokio::select! {
            biased;
            output = &mut future => return Some(output),
            () = &mut awake => return None,
            _ = check.tick() => {
                let passed = clock().duration_since(began).unwrap_or(Duration::ZERO);
                if passed >= limit {
                    return None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A wall clock the test moves by hand.
    fn hand_clock() -> (WallClock, Arc<Mutex<SystemTime>>) {
        let now = Arc::new(Mutex::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(1 << 30),
        ));
        let read = now.clone();
        (Arc::new(move || *read.lock().expect("clock")), now)
    }

    const LIMIT: Duration = Duration::from_secs(300);
    // The timer rounds a deadline up to the next millisecond.
    const TIMER_RESOLUTION: Duration = Duration::from_millis(1);

    /// Run `within` on a future that never completes, moving the wall clock by
    /// `jump` (forward or back) after `at` of awake time. Returns the awake time
    /// the bound took. Time is paused, so awake time moves only as timers fire.
    async fn bound_took(at: Duration, forward: bool, jump: Duration) -> Duration {
        let (clock, now) = hand_clock();
        let begun = Instant::now();
        let mover = tokio::spawn(async move {
            tokio::time::sleep(at).await;
            let mut now = now.lock().expect("clock");
            *now = if forward { *now + jump } else { *now - jump };
        });
        let ended = within(LIMIT, &clock, std::future::pending::<()>()).await;
        assert_eq!(ended, None);
        mover.await.expect("mover");
        begun.elapsed()
    }

    #[tokio::test(start_paused = true)]
    async fn a_bound_ends_at_the_next_check_once_wall_time_passed_its_limit_while_awake_time_did_not()
     {
        let took = bound_took(Duration::from_millis(2500), true, LIMIT).await;
        assert!(
            took >= Duration::from_secs(3) && took <= Duration::from_secs(3) + TIMER_RESOLUTION,
            "ended after {took:?} awake, not at the first check after the wall clock moved"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_bound_does_not_end_while_wall_time_is_short_of_its_limit() {
        let took = bound_took(
            Duration::from_millis(2500),
            true,
            LIMIT - Duration::from_secs(10),
        )
        .await;
        // 2.5 s awake plus a jump of 290 s reaches 300 s of wall time only if
        // the hand clock also moved with awake time, which it does not.
        assert!(
            took >= LIMIT && took <= LIMIT + TIMER_RESOLUTION,
            "ended after {took:?} awake"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_wall_clock_set_back_does_not_end_a_bound_early_or_keep_it_past_its_awake_limit() {
        let took = bound_took(
            Duration::from_millis(2500),
            false,
            Duration::from_secs(3600),
        )
        .await;
        assert!(
            took >= LIMIT && took <= LIMIT + TIMER_RESOLUTION,
            "ended after {took:?} awake, not at the awake-time limit of {LIMIT:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_bound_ends_at_its_awake_limit_when_the_wall_clock_does_not_move() {
        let took = bound_took(Duration::from_secs(1), true, Duration::ZERO).await;
        assert!(
            took >= LIMIT && took <= LIMIT + TIMER_RESOLUTION,
            "ended after {took:?} awake, not at the awake-time limit of {LIMIT:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_ready_future_is_returned_even_with_a_limit_of_zero() {
        let (clock, _now) = hand_clock();
        let got = within(Duration::ZERO, &clock, std::future::ready(7)).await;
        assert_eq!(got, Some(7));
        let none = within(Duration::ZERO, &clock, std::future::pending::<()>()).await;
        assert_eq!(none, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_future_kept_by_reference_can_still_be_awaited_after_the_limit() {
        let (clock, _now) = hand_clock();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        assert!(
            within(Duration::from_secs(1), &clock, &mut rx)
                .await
                .is_none()
        );
        tx.send(5).expect("the receiver is still open");
        assert_eq!(rx.await, Ok(5));
    }
}

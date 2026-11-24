//! Live progress events, for each provider, against the fakes.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{Rig, both, is_running, run_checked};
use crate::contract::outcome::Outcome;
use crate::contract::request::Provider;
use crate::provider::{ProgressEvent, ProgressSink, ProviderOptions};

fn recording(options: &mut ProviderOptions) -> Arc<Mutex<Vec<ProgressEvent>>> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink_events = events.clone();
    let sink: ProgressSink = Arc::new(move |event| {
        sink_events.lock().expect("lock").push(event);
    });
    options.progress = Some(sink);
    events
}

fn kinds(events: &[ProgressEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| match e {
            ProgressEvent::Provider { kind, .. } => Some(kind.as_str()),
            _ => None,
        })
        .collect()
}

async fn a_run_reports_started_provider_events_and_finished_in_order(provider: Provider) {
    let rig = Rig::new();
    let mut options = rig.options(provider);
    let events = recording(&mut options);
    let result = run_checked(rig.request(provider, "success"), options).await;
    assert_eq!(result.outcome, Outcome::Completed);

    let events = events.lock().expect("lock").clone();
    assert!(
        matches!(events.first(), Some(ProgressEvent::Started { provider: p, .. }) if *p == provider),
        "{events:?}"
    );
    assert_eq!(
        events.last(),
        Some(&ProgressEvent::Finished {
            outcome: Outcome::Completed
        })
    );
    let seen = kinds(&events);
    let expected = match provider {
        Provider::Claude => "result",
        Provider::Codex => "turn.completed",
    };
    assert_eq!(seen.last(), Some(&expected), "{events:?}");
    assert!(seen.len() >= 3, "{events:?}");
    rig.assert_nothing_left().await;
}
both!(
    reports_started_provider_events_and_finished,
    a_run_reports_started_provider_events_and_finished_in_order
);

async fn a_denied_request_is_reported_before_finished(provider: Provider) {
    let rig = Rig::new();
    let mut options = rig.options(provider);
    let events = recording(&mut options);
    let result = run_checked(rig.request(provider, "denied-completed"), options).await;
    assert_eq!(result.permissions.denied.len(), 1);

    let events = events.lock().expect("lock").clone();
    let n = events.len();
    assert!(
        matches!(&events[n - 2], ProgressEvent::Denied { request } if request.contains("rm -rf")),
        "{events:?}"
    );
    assert!(matches!(&events[n - 1], ProgressEvent::Finished { .. }));
    rig.assert_nothing_left().await;
}
both!(
    reports_a_denial_before_finished,
    a_denied_request_is_reported_before_finished
);

async fn a_refused_run_reports_only_finished(provider: Provider) {
    let rig = Rig::new();
    let mut options = rig.options(provider);
    let events = recording(&mut options);
    let mut request = rig.request(provider, "success");
    request.timeout_secs = 0;
    let result = crate::provider::run(request, options).await;
    assert_eq!(result.outcome, Outcome::Refused);
    assert_eq!(
        *events.lock().expect("lock"),
        [ProgressEvent::Finished {
            outcome: Outcome::Refused
        }]
    );
}
both!(
    reports_only_finished_for_a_refusal,
    a_refused_run_reports_only_finished
);

async fn no_summary_carries_a_command_or_the_environment(provider: Provider) {
    let rig = Rig::new();
    let mut options = rig.options(provider);
    options
        .env_set
        .push(("MY_SERVICE_API_TOKEN".into(), "tok-value-123456".into()));
    let events = recording(&mut options);
    let target = rig.work.join("out.txt");
    let prompt = format!("tool-write:{}", target.display());
    let _ = run_checked(rig.request(provider, &prompt), options).await;
    // A denial names its target, as `permissions.denied` does; only the
    // summaries of the provider's own events are checked here.
    let text: String = events
        .lock()
        .expect("lock")
        .iter()
        .filter(|e| {
            matches!(
                e,
                ProgressEvent::Provider { .. } | ProgressEvent::Started { .. }
            )
        })
        .map(|e| format!("{e:?}"))
        .collect();
    assert!(!text.contains("tok-value-123456"), "{text}");
    // The Codex fake runs `touch TARGET`; the command must not be reported.
    assert!(!text.contains("touch"), "{text}");
}
both!(
    summaries_hold_no_command_and_no_environment_value,
    no_summary_carries_a_command_or_the_environment
);

struct ReleaseSink(Arc<AtomicBool>);

impl Drop for ReleaseSink {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn release_after_provider_exit(
    _release: ReleaseSink,
    pid_file: &Path,
    give_up: Duration,
    is_running: impl Fn(i32) -> Option<bool>,
) -> Result<(), &'static str> {
    let began = Instant::now();
    while began.elapsed() < give_up {
        if let Ok(text) = std::fs::read_to_string(pid_file)
            && let Ok(pid) = text.trim().parse::<i32>()
        {
            while began.elapsed() < give_up {
                match is_running(pid) {
                    Some(false) => return Ok(()),
                    None => return Err("could not check whether the fake provider is running"),
                    Some(true) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
            return Err("the fake provider did not exit within the wait bound");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err("the fake provider did not record its PID within the wait bound")
}

async fn a_slow_sink_loses_output_and_the_run_says_so(provider: Provider) {
    // Proves: when the sink cannot take events the output is dropped whole
    // chunks at a time, counted, announced before `Finished` and noted in the
    // result. Not speed dependent: the sink is stuck on its first provider event until
    // the fake has exited, the tap's queue holds two chunks, and the fake's
    // output (about 1.8 MB, read in chunks of at most 64 KB, with at most one
    // pipe's worth unread when it exits) is at least 25 chunks, so at least 20
    // are dropped however fast anything runs.
    let rig = Rig::new();
    let mut options = rig.options(provider);
    options.tap_queue_chunks = Some(2);
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink_events = events.clone();
    let release = Arc::new(AtomicBool::new(false));
    let sink_release = release.clone();
    let sink: ProgressSink = Arc::new(move |event| {
        // Only the events built from the provider's output, which come from the
        // tap's thread, are held; the run's own events come from the run itself
        // and must not wait.
        if matches!(event, ProgressEvent::Provider { .. }) {
            while !sink_release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        sink_events.lock().expect("lock").push(event);
    });
    options.progress = Some(sink);
    // Released from a thread of its own: the run cannot end while the sink is
    // stuck, so nothing on the runtime can be the one to release it.
    let pid_file = rig.record.join("pid");
    let release = ReleaseSink(release);
    let releaser = std::thread::spawn(move || {
        release_after_provider_exit(release, &pid_file, Duration::from_secs(120), is_running)
    });
    let result = run_checked(rig.request(provider, "chatty"), options).await;
    releaser
        .join()
        .expect("the releasing thread")
        .expect("waiting for the fake provider to exit");
    assert_eq!(result.outcome, Outcome::Completed);

    let events = events.lock().expect("lock").clone();
    let n = events.len();
    let chunks = match &events[n - 2] {
        ProgressEvent::Dropped { chunks } => *chunks,
        other => panic!("expected Dropped before Finished, got {other:?}"),
    };
    assert!(chunks > 0);
    assert!(matches!(&events[n - 1], ProgressEvent::Finished { .. }));
    assert!(
        result
            .provider_notes
            .iter()
            .any(|note| note.contains(&format!("{chunks} chunks of provider output"))),
        "{:?}",
        result.provider_notes
    );
    rig.assert_nothing_left().await;
}
both!(
    a_slow_sink_gets_a_dropped_event_and_a_note,
    a_slow_sink_loses_output_and_the_run_says_so
);

#[test]
fn slow_sink_is_released_promptly_when_ps_cannot_start() {
    let rig = Rig::new();
    let pid_file = rig.record.join("pid");
    std::fs::write(&pid_file, "123").expect("write a fake PID");
    let missing_ps = rig.record.join("missing-ps");
    let release = Arc::new(AtomicBool::new(false));
    let guard = ReleaseSink(release.clone());
    let (done, result) = std::sync::mpsc::channel();
    let releaser = std::thread::spawn(move || {
        let result =
            release_after_provider_exit(guard, &pid_file, Duration::from_secs(120), |pid| {
                super::ps_is_running(&missing_ps, pid)
            });
        let _ = done.send(result);
    });

    assert_eq!(
        result
            .recv_timeout(Duration::from_secs(10))
            .expect("the releaser should finish promptly when ps cannot start"),
        Err("could not check whether the fake provider is running")
    );
    releaser.join().expect("the releasing thread");
    assert!(release.load(Ordering::SeqCst), "the sink is still blocked");
}

#[test]
fn slow_sink_is_released_if_the_releaser_panics() {
    let rig = Rig::new();
    let pid_file = rig.record.join("pid");
    std::fs::write(&pid_file, "123").expect("write a fake PID");
    let release = Arc::new(AtomicBool::new(false));
    let guard = ReleaseSink(release.clone());
    let result = std::panic::catch_unwind(move || {
        release_after_provider_exit(guard, &pid_file, Duration::from_secs(10), |_| {
            panic!("liveness check panicked")
        })
    });

    assert!(result.is_err(), "the liveness check should be called");
    assert!(release.load(Ordering::SeqCst), "the sink is still blocked");
}

fn messages(events: &[ProgressEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| match e {
            ProgressEvent::Message { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

async fn message_text_is_sent_only_when_asked_for(provider: Provider) {
    let rig = Rig::new();
    let mut options = rig.options(provider);
    let quiet = recording(&mut options);
    run_checked(rig.request(provider, "success"), options).await;
    assert!(
        messages(&quiet.lock().expect("lock")).is_empty(),
        "text must be opt-in"
    );

    let mut options = rig.options(provider);
    options.progress_text = true;
    let loud = recording(&mut options);
    run_checked(rig.request(provider, "success"), options).await;
    let events = loud.lock().expect("lock").clone();
    // The commentary message of the fake, then the final answer for Codex,
    // which writes its answer as a message item too.
    let texts = messages(&events);
    assert_eq!(texts.first(), Some(&"working"), "{events:?}");
    let first_text = events
        .iter()
        .position(|e| matches!(e, ProgressEvent::Message { .. }))
        .expect("a message event");
    assert!(
        matches!(&events[first_text - 1], ProgressEvent::Provider { summary, .. } if summary == "message: 7 chars"),
        "the length event comes first: {events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(ProgressEvent::Finished { .. })
    ));
    rig.assert_nothing_left().await;
}
both!(
    message_text_arrives_as_message_events_only_on_request,
    message_text_is_sent_only_when_asked_for
);

async fn message_text_has_credentials_removed_and_is_bounded(provider: Provider) {
    let rig = Rig::new();
    let mut options = rig.options(provider);
    options.progress_text = true;
    options
        .env_set
        .push(("MY_SERVICE_API_TOKEN".into(), "tok-value-123456".into()));
    let events = recording(&mut options);
    run_checked(rig.request(provider, "secret-message"), options).await;
    let events = events.lock().expect("lock").clone();
    let texts = messages(&events);
    // The first message held the token value and a key; both are gone and the
    // text around them is not.
    let secret = texts
        .iter()
        .find(|t| t.starts_with("token="))
        .expect("the message that held the secret");
    assert!(!secret.contains("tok-value-123456"));
    assert!(!secret.contains("sk-ant-"));
    // Another was 5000 characters and is cut.
    let long = texts
        .iter()
        .find(|t| t.starts_with("xxxx"))
        .expect("the long message");
    assert_eq!(
        long.chars().count(),
        crate::provider::limits::PROGRESS_TEXT_CHARS + 3
    );
    rig.assert_nothing_left().await;
}
both!(
    message_text_is_redacted_and_bounded,
    message_text_has_credentials_removed_and_is_bounded
);

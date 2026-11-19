//! Live progress events, for each provider, against the fakes.

use std::sync::{Arc, Mutex};

use super::{Rig, both, run_checked};
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

async fn a_slow_sink_loses_output_and_the_run_says_so(provider: Provider) {
    let rig = Rig::new();
    let mut options = rig.options(provider);
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink_events = events.clone();
    let sink: ProgressSink = Arc::new(move |event| {
        std::thread::sleep(std::time::Duration::from_millis(10));
        sink_events.lock().expect("lock").push(event);
    });
    options.progress = Some(sink);
    let result = run_checked(rig.request(provider, "chatty"), options).await;
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

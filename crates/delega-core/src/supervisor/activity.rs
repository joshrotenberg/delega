//! What a run has done so far, kept from the provider's progress events.
//!
//! The provider layer calls a sink from the task that reads the provider's
//! output, so recording must be quick and must never wait on the supervisor.
//! Each run owns one bounded buffer behind a mutex, the sink writes to it, and
//! a read copies out of it. Nothing here goes through the supervisor's task.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use serde::Serialize;

use crate::contract::outcome::Outcome;
use crate::process::rfc3339_utc;
use crate::provider::ProgressEvent;

use super::limits::{MAX_ACTIVITY, MAX_PARTIAL_TEXT_CHARS};

/// One thing the run did, as the provider layer reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActivityEntry {
    /// Position in the supervisor's one cursor sequence. Pass the largest one
    /// you have seen as `after_cursor` to `read` to get only later entries.
    pub cursor: u64,
    /// When it was reported, RFC 3339 in UTC.
    pub at: String,
    /// `started`, `denied`, `dropped`, `finished`, or the provider's own event
    /// type such as `assistant` or `item.completed`.
    pub kind: String,
    /// A short description. An agent message is shown by its length, never its
    /// text; the text is in the partial text.
    pub summary: String,
}

/// A copy of a run's buffer.
#[derive(Debug, Clone, Default)]
pub(super) struct Snapshot {
    pub text: String,
    pub text_truncated: bool,
    pub entries: Vec<ActivityEntry>,
    /// Entries reported over the whole run.
    pub total: u64,
    /// Entries dropped from the front to stay within [`MAX_ACTIVITY`].
    pub omitted: u64,
}

#[derive(Debug, Default)]
struct Inner {
    text: String,
    text_truncated: bool,
    entries: VecDeque<ActivityEntry>,
    total: u64,
    omitted: u64,
}

/// The bounded record of one run's progress.
#[derive(Debug, Default)]
pub(super) struct Activity(Mutex<Inner>);

impl Activity {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record one provider event. `cursor` is the supervisor's shared counter.
    pub fn record(&self, event: ProgressEvent, cursor: &AtomicU64) {
        let (kind, summary) = match event {
            ProgressEvent::Started { provider, program } => (
                "started".to_string(),
                format!("{} ({program})", provider_word(provider)),
            ),
            ProgressEvent::Provider { kind, summary } => (kind, summary),
            ProgressEvent::Denied { request } => ("denied".to_string(), request),
            ProgressEvent::Dropped { chunks } => (
                "dropped".to_string(),
                format!("{chunks} chunks of provider output were not reported"),
            ),
            ProgressEvent::Finished { outcome } => {
                ("finished".to_string(), outcome_word(outcome).to_string())
            }
            ProgressEvent::Message { text } => {
                self.append_text(&text);
                return;
            }
        };
        let entry = ActivityEntry {
            cursor: cursor.fetch_add(1, Ordering::SeqCst) + 1,
            at: rfc3339_utc(SystemTime::now()),
            kind,
            summary,
        };
        let mut inner = self.lock();
        inner.total += 1;
        if inner.entries.len() >= MAX_ACTIVITY {
            inner.entries.pop_front();
            inner.omitted += 1;
        }
        inner.entries.push_back(entry);
    }

    /// Add one agent message to the partial text, keeping the end of it when
    /// the bound is passed.
    fn append_text(&self, text: &str) {
        let mut inner = self.lock();
        if !inner.text.is_empty() {
            inner.text.push_str("\n\n");
        }
        inner.text.push_str(text);
        let chars = inner.text.chars().count();
        if chars > MAX_PARTIAL_TEXT_CHARS {
            let cut = inner
                .text
                .char_indices()
                .nth(chars - MAX_PARTIAL_TEXT_CHARS)
                .map_or(0, |(i, _)| i);
            inner.text.drain(..cut);
            inner.text_truncated = true;
        }
    }

    /// The partial text so far, used when a run ends without a result of its
    /// own.
    pub fn text(&self) -> String {
        self.lock().text.clone()
    }

    /// A copy of the buffer.
    pub fn snapshot(&self) -> Snapshot {
        let inner = self.lock();
        Snapshot {
            text: inner.text.clone(),
            text_truncated: inner.text_truncated,
            entries: inner.entries.iter().cloned().collect(),
            total: inner.total,
            omitted: inner.omitted,
        }
    }
}

fn provider_word(provider: crate::contract::request::Provider) -> &'static str {
    match provider {
        crate::contract::request::Provider::Claude => "claude",
        crate::contract::request::Provider::Codex => "codex",
    }
}

fn outcome_word(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Completed => "completed",
        Outcome::Failed => "failed",
        Outcome::TimedOut => "timed_out",
        Outcome::Interrupted => "interrupted",
        Outcome::Refused => "refused",
    }
}

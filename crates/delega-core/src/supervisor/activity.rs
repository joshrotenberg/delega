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

use schemars::JsonSchema;
use serde::Serialize;

use crate::contract::outcome::Outcome;
use crate::process::rfc3339_utc;
use crate::provider::ProgressEvent;

use super::limits::{MAX_ACTIVITY, MAX_PARTIAL_TEXT_CHARS};

/// One thing the run did, as the provider layer reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
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
    /// text; the text is in the partial text. Consecutive provider `system`
    /// events with the same summary are one entry, updated in place, whose
    /// summary ends in ` (N in a row)` once N is 2 or more, for example
    /// `thinking_tokens (37 in a row)`.
    pub summary: String,
}

/// A copy of a run's buffer.
#[derive(Debug, Clone, Default)]
pub(super) struct Snapshot {
    pub text: String,
    pub text_truncated: bool,
    pub entries: Vec<ActivityEntry>,
    /// Events reported over the whole run, counting each event of a
    /// coalesced run of `system` events.
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
    /// When the last entry stands for a run of identical provider `system`
    /// events: their summary as the provider reported it, and how many.
    system_run: Option<(String, u64)>,
}

/// The bounded record of one run's progress.
#[derive(Debug, Default)]
pub(super) struct Activity(Mutex<Inner>);

impl Activity {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record one provider event. `cursor` is the supervisor's shared counter.
    ///
    /// A provider `system` event with the same summary as the `system` event
    /// just before it replaces that entry instead of adding one: Claude Code
    /// reports `thinking_tokens` about once a second, and one entry each would
    /// push tool use, tool results and messages out of the bounded window. The
    /// replacement takes a new cursor and time, and its summary says how many
    /// arrived in a row. Other provider events and delega's own entries are
    /// never merged. `total` still counts every event.
    pub fn record(&self, event: ProgressEvent, cursor: &AtomicU64) {
        let mut coalescible = false;
        let (kind, summary) = match event {
            ProgressEvent::Started { provider, program } => (
                "started".to_string(),
                format!("{} ({program})", provider_word(provider)),
            ),
            ProgressEvent::Provider { kind, summary } => {
                coalescible = kind == "system";
                (kind, summary)
            }
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
        // The sequence number is taken and the entry pushed under one lock. A
        // reader that has seen a cursor and then takes this lock therefore finds
        // every entry numbered at or below it: none can still be on its way in.
        // A coalesced entry is replaced under the same lock with a cursor above
        // every cursor already handed out, so a reader that saw the old one
        // and passes it as `after` still receives the new one.
        let mut inner = self.lock();
        inner.total += 1;
        let repeats = match &inner.system_run {
            Some((last, count)) if coalescible && *last == summary => Some(count + 1),
            _ => None,
        };
        let shown = match repeats {
            Some(count) => format!("{summary} ({count} in a row)"),
            None => summary.clone(),
        };
        let entry = ActivityEntry {
            cursor: cursor.fetch_add(1, Ordering::SeqCst) + 1,
            at: rfc3339_utc(SystemTime::now()),
            kind,
            summary: shown,
        };
        inner.system_run = coalescible.then(|| (summary, repeats.unwrap_or(1)));
        if repeats.is_some() {
            // The run's entry is the last one: replace it, keeping the length.
            inner.entries.pop_back();
        } else if inner.entries.len() >= MAX_ACTIVITY {
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

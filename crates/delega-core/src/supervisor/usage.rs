//! A follow-up's own usage, worked out from session totals.
//!
//! Some providers report only the session's totals on a resumed run (Codex
//! reports the thread's totals on every turn). A worker keeps the latest
//! `session_usage` its runs reported, and a later run of the same session that
//! reported totals but no run token counts gets the difference as its `usage`.
//! The difference covers everything since the previous reported totals,
//! including a run in between that reported none, such as a failed one.

use crate::contract::result::{RunResult, Usage};

/// Fill `result.usage` from the difference between its `session_usage` and
/// `previous`, the worker's latest totals, when the result has session totals
/// and no run token counts. Fields the result already has are kept.
pub(super) fn fill_from_totals(result: &mut RunResult, previous: Option<&Usage>) {
    let (Some(previous), Some(current)) = (previous, result.session_usage.as_ref()) else {
        return;
    };
    if result.usage.as_ref().is_some_and(has_tokens) {
        return;
    }
    let Some(diff) = difference(previous, current) else {
        return;
    };
    let own = result.usage.take().unwrap_or_default();
    result.usage = Some(Usage {
        input_tokens: own.input_tokens.or(diff.input_tokens),
        output_tokens: own.output_tokens.or(diff.output_tokens),
        cached_tokens: own.cached_tokens.or(diff.cached_tokens),
        reasoning_tokens: own.reasoning_tokens.or(diff.reasoning_tokens),
        cost_usd: own.cost_usd.or(diff.cost_usd),
    });
}

/// Whether a usage has any token count.
fn has_tokens(usage: &Usage) -> bool {
    usage.input_tokens.is_some()
        || usage.output_tokens.is_some()
        || usage.cached_tokens.is_some()
        || usage.reasoning_tokens.is_some()
}

/// `current` minus `previous`, field by field, over the fields both have.
/// `None` when any such field went down, since then the totals are not of one
/// growing session, or when they share no field.
pub(super) fn difference(previous: &Usage, current: &Usage) -> Option<Usage> {
    fn count(previous: Option<u64>, current: Option<u64>) -> Result<Option<u64>, ()> {
        match (previous, current) {
            (Some(p), Some(c)) if c >= p => Ok(Some(c - p)),
            (Some(_), Some(_)) => Err(()),
            _ => Ok(None),
        }
    }
    let cost = match (previous.cost_usd, current.cost_usd) {
        (Some(p), Some(c)) if c >= p => Some(c - p),
        (Some(_), Some(_)) => return None,
        _ => None,
    };
    let diff = Usage {
        input_tokens: count(previous.input_tokens, current.input_tokens).ok()?,
        output_tokens: count(previous.output_tokens, current.output_tokens).ok()?,
        cached_tokens: count(previous.cached_tokens, current.cached_tokens).ok()?,
        reasoning_tokens: count(previous.reasoning_tokens, current.reasoning_tokens).ok()?,
        cost_usd: cost,
    };
    (has_tokens(&diff) || diff.cost_usd.is_some()).then_some(diff)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(input: u64, output: u64, cached: u64) -> Usage {
        Usage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            cached_tokens: Some(cached),
            ..Usage::default()
        }
    }

    #[test]
    fn difference_subtracts_each_field_both_totals_have() {
        let diff = difference(&tokens(16_229, 603, 13_440), &tokens(33_074, 608, 26_880));
        assert_eq!(diff, Some(tokens(16_845, 5, 13_440)));
    }

    #[test]
    fn difference_is_none_when_any_total_went_down() {
        assert_eq!(difference(&tokens(100, 10, 5), &tokens(200, 9, 6)), None);
    }

    #[test]
    fn difference_is_none_when_cost_went_down() {
        let previous = Usage {
            cost_usd: Some(0.5),
            ..tokens(1, 1, 1)
        };
        let current = Usage {
            cost_usd: Some(0.25),
            ..tokens(2, 2, 2)
        };
        assert_eq!(difference(&previous, &current), None);
    }

    #[test]
    fn difference_leaves_out_a_field_only_one_total_has() {
        let previous = Usage {
            input_tokens: Some(10),
            ..Usage::default()
        };
        let current = Usage {
            input_tokens: Some(15),
            reasoning_tokens: Some(3),
            ..Usage::default()
        };
        let diff = difference(&previous, &current).expect("a difference");
        assert_eq!(diff.input_tokens, Some(5));
        assert_eq!(diff.reasoning_tokens, None);
    }

    #[test]
    fn difference_of_totals_with_no_shared_field_is_none() {
        let previous = Usage {
            input_tokens: Some(10),
            ..Usage::default()
        };
        let current = Usage {
            output_tokens: Some(15),
            ..Usage::default()
        };
        assert_eq!(difference(&previous, &current), None);
    }
}

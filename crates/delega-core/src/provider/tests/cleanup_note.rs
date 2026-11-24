//! The cleanup note a result carries: without a keeper, or with one on macOS.

use super::{Rig, both, run_checked};
use crate::contract::request::Provider;
use crate::provider::run::cleanup_note;
use crate::provider::{MACOS_KEEPER_NOTE, NO_KEEPER_NOTE};

#[test]
fn only_a_kept_run_on_macos_gets_the_macos_scope_note() {
    assert_eq!(cleanup_note(true, true), Some(MACOS_KEEPER_NOTE));
    assert_eq!(cleanup_note(true, false), None);
    assert_eq!(cleanup_note(false, true), Some(NO_KEEPER_NOTE));
    assert_eq!(cleanup_note(false, false), Some(NO_KEEPER_NOTE));
}

async fn unkept(provider: Provider) {
    let rig = Rig::new();
    let options = rig.options(provider);
    assert!(options.keeper.is_none(), "the rig runs without a keeper");
    let result = run_checked(rig.request(provider, "success"), options).await;
    let notes = &result.provider_notes;
    assert!(notes.iter().any(|n| n == NO_KEEPER_NOTE), "{notes:?}");
    assert!(!notes.iter().any(|n| n == MACOS_KEEPER_NOTE), "{notes:?}");
    rig.assert_nothing_left().await;
}

both!(an_unkept_run_names_its_gaps_and_not_the_macos_scope, unkept);

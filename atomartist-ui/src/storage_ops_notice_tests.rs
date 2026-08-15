//! Unit tests for the notice queue's consecutive-duplicate suppression
//! (`crate::storage_ops::push_notice`).
//!
//! Their own file, like `storage_ops_quiet_tests.rs`: `storage_ops_tests.rs`
//! is within a few lines of the project's 800-line cap, and this is a
//! self-contained rule with its own reason to change.

use super::storage_ops_tests::state;
use super::*;

/// One failure now reaches the queue by two legitimate routes — the
/// operation's own `notify` and a dialog provider whose `show_error`
/// posts here too — and the user must not read the same sentence twice.
#[test]
fn a_message_identical_to_the_last_one_is_not_queued_twice() {
    let state = state();
    state.notify(NoticeLevel::Error, "Save failed: disk on fire");
    state.notify(NoticeLevel::Error, "Save failed: disk on fire");

    assert_eq!(
        state.drain_notices().len(),
        1,
        "consecutive duplicates collapse to one"
    );
}

/// Only *consecutive* duplicates: the same message after something else
/// was said is news, and severity is part of the identity.
#[test]
fn a_repeat_after_another_message_is_still_news() {
    let state = state();
    state.notify(NoticeLevel::Error, "Export failed");
    state.notify(NoticeLevel::Info, "Exported bracket.stl");
    state.notify(NoticeLevel::Error, "Export failed");
    // Same text, different level — a different message.
    state.notify(NoticeLevel::Info, "Export failed");

    let drained = state.drain_notices();
    assert_eq!(drained.len(), 4);
    assert_eq!(drained[3].level, NoticeLevel::Info);
}

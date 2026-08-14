//! The deferred-close decision — the one piece of `main.rs`'s
//! `CloseRequested` / `AboutToWait` handshake that can be tested without
//! a window.
//!
//! Shape of the handshake (see `main.rs`): answering **Save** to the
//! close prompt submits an asynchronous write and hands the permission to
//! close to that write's continuation, which sets an `AtomicBool`. With
//! the `file:` provider the job is already settled, so the flag is set
//! before `CloseRequested` returns and the window closes on that event.
//! With a slow provider the window stays open and `AboutToWait` finishes
//! the close once the pump delivers the result.
//!
//! That gap is where this module earns its keep: the user can keep
//! editing while the save is in flight. Honouring a flag set for the
//! document *as it was at the click* would then discard the newer edits
//! without a word — the exact failure the unsaved-changes gate exists to
//! prevent. So the flag is a request, re-validated here against the live
//! dirty state before the shell acts on it.

/// What `AboutToWait` should do with the deferred-close flag this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferredClose {
    /// No close is pending — carry on painting.
    NotRequested,
    /// The save the user asked for landed and the document is still
    /// clean: shut down.
    Close,
    /// The document went dirty again after the user chose Save, so the
    /// permission they gave no longer covers the current work. Clear the
    /// flag and stay open; the new edits need their own gate.
    CancelledByNewEdits,
}

/// Decide what to do with a set close flag.
///
/// Pure on purpose — the winit half (calling `elwt.exit()`) has no
/// headless entry point, but this decision does.
pub fn deferred_close_decision(flag_set: bool, has_unsaved_changes: bool) -> DeferredClose {
    match (flag_set, has_unsaved_changes) {
        (false, _) => DeferredClose::NotRequested,
        (true, false) => DeferredClose::Close,
        (true, true) => DeferredClose::CancelledByNewEdits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_flag_never_closes() {
        assert_eq!(
            deferred_close_decision(false, false),
            DeferredClose::NotRequested
        );
        assert_eq!(
            deferred_close_decision(false, true),
            DeferredClose::NotRequested
        );
    }

    #[test]
    fn a_confirmed_save_with_a_clean_document_closes() {
        assert_eq!(deferred_close_decision(true, false), DeferredClose::Close);
    }

    /// The bug this exists for: X → Save → keep editing → the *old* save
    /// lands → the window closes, discarding the newer edits.
    #[test]
    fn edits_made_while_the_save_was_in_flight_cancel_the_close() {
        assert_eq!(
            deferred_close_decision(true, true),
            DeferredClose::CancelledByNewEdits
        );
    }
}

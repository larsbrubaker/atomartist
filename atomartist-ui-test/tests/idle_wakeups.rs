//! What the frame loop does while a storage operation is *pending and
//! doing nothing* — step 6g-1 of `docs/file-browser-design.md` §5c.
//!
//! No NodeDesigner counterpart: the ancestor repainted from the browser's
//! own rAF loop and never had to decide when to sleep.
//!
//! The file-picker modal is the worst case for the storage pump's
//! keep-alive: its `JobOp` stays pending for as long as the user takes to
//! answer — seconds, minutes — while absolutely nothing about it changes.
//! A keep-alive that asks for a frame just because *something is queued*
//! therefore pins the app at full framerate for the whole time the dialog
//! sits idle on screen.
//!
//! These tests replay `demo-native`'s idle decision (pump, then
//! `animation::wants_draw() || app.wants_draw()`) over a fixed number of
//! loop turns and count how many of them would have painted. They are the
//! automated half of the measurement in the step's report; the other half
//! is `ATOMARTIST_FPS_LOG=1` in the real shell.

use std::sync::Arc;

use atomartist_storage::{MemoryProvider, StorageProvider, StorageRegistry};
use atomartist_ui::{fresh_state_with_starter_graph_and_storage, AppState};
use atomartist_ui_test::TestHarness;

/// Loop turns per measurement — enough that a per-frame keep-alive is
/// unmistakable and few enough to stay instant.
const TURNS: usize = 120;

fn memory_registry() -> Arc<StorageRegistry> {
    let mut registry = StorageRegistry::new();
    registry
        .register(Arc::new(MemoryProvider::new("mem", "Test Memory")) as Arc<dyn StorageProvider>)
        .expect("fresh registry accepts the memory provider");
    Arc::new(registry)
}

/// Run `turns` of the native shell's idle loop and report how many of them
/// would have painted a frame.
///
/// Mirrors `demo-native`'s `AboutToWait` arm: clear the per-frame draw
/// flags (agg-gui does this at the top of `App::paint`), pump storage, then
/// ask both draw sources whether another frame is owed. A turn that says
/// yes is charged a real layout + paint, so widget-driven `needs_draw` is
/// measured the same way the shell would see it.
fn painted_turns(h: &mut TestHarness, turns: usize) -> usize {
    let mut painted = 0;
    for _ in 0..turns {
        agg_gui::animation::clear_draw_request();
        h.state().pump_storage();
        if agg_gui::animation::wants_draw() || h.app().wants_draw() {
            painted += 1;
            h.frame();
            h.paint_once();
        }
    }
    painted
}

/// An open, untouched file-picker modal must let the loop sleep.
///
/// The picker's `JobOp` is pending the whole time and *cannot* change
/// without a user event, so every frame this costs is a frame spent
/// redrawing an identical window.
#[test]
fn an_idle_open_file_dialog_does_not_pin_the_frame_loop() {
    let storage = memory_registry();
    let mut h = TestHarness::with_modal_dialogs(fresh_state_with_starter_graph_and_storage(
        storage.clone(),
    ));
    h.menu_action("file.open");
    assert!(h.browser_modal().is_open(), "File → Open shows the browser");
    assert_eq!(
        h.state().pending_op_count(),
        1,
        "the picker's job is the one loud operation in flight"
    );

    // Let the modal's own first-frame work (directory listing, thumbnail
    // round, focus) settle before measuring: those are real changes and
    // are allowed to paint.
    for _ in 0..12 {
        h.state().pump_storage();
        h.frame();
        h.paint_once();
    }

    let painted = painted_turns(&mut h, TURNS);
    println!("idle open dialog: {painted}/{TURNS} turns painted");
    assert!(
        painted * 10 <= TURNS,
        "an idle dialog painted {painted} of {TURNS} idle turns — the storage \
         pump (or the status bar) is keeping the loop awake with nothing to show"
    );
    assert!(
        h.browser_modal().is_open(),
        "and the dialog is still up afterwards"
    );
}

/// The other half of the contract: sleeping must not lose a completion.
/// Escape settles the picker job on the main thread — exactly as a click
/// on Cancel or OK does — and the very next pump applies the continuation,
/// even after the loop has spent a long stretch idle.
#[test]
fn answering_the_dialog_is_still_applied_on_the_next_pump() {
    let storage = memory_registry();
    let state: AppState = fresh_state_with_starter_graph_and_storage(storage.clone());
    let mut h = TestHarness::with_modal_dialogs(state);
    h.menu_action("file.open");
    let painted_before = painted_turns(&mut h, TURNS);

    h.key_down(agg_gui::Key::Escape);
    // The close is noticed on the next layout pass (`settle_if_closed`),
    // which is where the completer publishes the "cancelled" answer.
    h.frame();
    // One pump is all the shell gets before it decides to sleep again.
    h.state().pump_storage();
    assert!(!h.browser_modal().is_open(), "Escape closed the dialog");
    assert_eq!(
        h.state().pending_op_count(),
        0,
        "the answered picker was applied on the next pump ({painted_before} \
         idle turns painted beforehand)"
    );
}

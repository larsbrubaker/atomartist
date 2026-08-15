//! Browser coverage for the WASM shell's persistence idle guard
//! (`demo_wasm::web_lifecycle`) — the piece that decides when it is safe
//! to write settings, and the DOM listeners that keep that decision from
//! wedging when a drag ends somewhere the canvas can't see.
//!
//! Same harness as `tests/web_settings.rs`; see that file's header (and
//! `atomartist-storage/README.md`) for the chromedriver invocation:
//!
//! ```text
//! cargo test -p demo-wasm --target wasm32-unknown-unknown --test web_lifecycle
//! ```

#![cfg(target_arch = "wasm32")]

use demo_wasm::web_lifecycle::{
    install_window_mouse_release_listener, mouse_idle, note_mouse_down, note_mouse_up,
    pressed_button_count, sync_mouse_buttons,
};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// The held-button count is thread-local and shared by every test in
/// this module (wasm tests run sequentially on one thread), so each test
/// starts from a known state.
fn reset_guard() {
    sync_mouse_buttons(0);
}

#[wasm_bindgen_test]
fn button_count_ignores_buttons_the_app_never_drags_with() {
    assert_eq!(pressed_button_count(0), 0);
    assert_eq!(pressed_button_count(0b001), 1, "left");
    assert_eq!(pressed_button_count(0b010), 1, "right");
    assert_eq!(pressed_button_count(0b101), 2, "left + middle");
    assert_eq!(pressed_button_count(0b111), 3, "all three");
    // Back / forward / eraser bits must not hold the guard closed.
    assert_eq!(pressed_button_count(0b1_1000), 0);
    assert_eq!(pressed_button_count(0b1_1001), 1);
}

#[wasm_bindgen_test]
fn a_balanced_press_and_release_leaves_the_guard_open() {
    reset_guard();
    assert!(mouse_idle());
    note_mouse_down();
    assert!(!mouse_idle(), "settings must not be written mid-drag");
    note_mouse_up();
    assert!(mouse_idle());
}

/// The regression this whole change exists for: a press inside the
/// canvas released over browser chrome delivers no canvas `mouseup`, so
/// the naive counter stays positive forever and settings stop
/// persisting for the rest of the session. The next `mousemove` carries
/// `buttons == 0` and must reopen the guard.
#[wasm_bindgen_test]
fn a_lost_mouseup_is_healed_by_the_next_move() {
    reset_guard();
    note_mouse_down();
    // ...release happens off-canvas; no `on_mouse_up` ever arrives.
    assert!(!mouse_idle());
    sync_mouse_buttons(0);
    assert!(mouse_idle(), "a move with no buttons held must un-wedge the guard");
}

#[wasm_bindgen_test]
fn a_move_with_a_button_still_down_keeps_the_guard_closed() {
    reset_guard();
    sync_mouse_buttons(0b001);
    assert!(!mouse_idle());
}

/// Even without any mouse movement, a release that reaches the page at
/// all must reopen the guard — that is what the window-level listener
/// is for. Driven here with a synthetic `pointerup`, dispatched on
/// `window` exactly as a real release would be.
#[wasm_bindgen_test]
fn a_window_level_release_reopens_the_guard() {
    reset_guard();
    install_window_mouse_release_listener();

    note_mouse_down();
    note_mouse_down();
    assert!(!mouse_idle());

    let window = web_sys::window().expect("window");
    let init = web_sys::MouseEventInit::new();
    init.set_bubbles(true);
    // No buttons remain held after this release.
    init.set_buttons(0);
    let event = web_sys::MouseEvent::new_with_mouse_event_init_dict("pointerup", &init)
        .expect("construct pointerup");
    window
        .dyn_ref::<web_sys::EventTarget>()
        .expect("window is an EventTarget")
        .dispatch_event(&event)
        .expect("dispatch");

    assert!(
        mouse_idle(),
        "a release seen only by the window listener must still clear the held count"
    );
}

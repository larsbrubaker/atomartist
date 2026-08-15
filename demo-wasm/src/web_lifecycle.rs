//! Browser lifecycle plumbing for the WASM shell: the mouse-held idle
//! guard, the diff-guarded settings `AutoSave`, and the page-hide flush.
//!
//! Split out of `lib.rs` (which owns the wgpu/App thread-locals and the
//! render loop) so the shell keeps to the 800-line file cap. The
//! division of labour is:
//!
//!   * `lib.rs` owns `APP` / `STATE` / `DEBUG` and the exported mouse
//!     entry points; it calls into here to update the held-button count
//!     and to tick persistence after a painted frame.
//!   * this module owns *when* it is safe to persist ([`mouse_idle`])
//!     and the DOM listeners that keep that answer honest
//!     ([`install_window_mouse_release_listener`]) or that force a final
//!     write before the page goes away ([`install_page_hide_flush`]).
//!
//! Everything here is pure Rust via `web-sys`; `index.html` carries no
//! lifecycle glue beyond forwarding canvas mouse events.

use std::cell::Cell;

use agg_gui::persistence::AutoSave;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

thread_local! {
    // Diff-guarded settings persistence, the web twin of demo-native's
    // `settings_auto_save`: ticked after each painted frame, writes the
    // composed blob to localStorage only when it changed.
    static SETTINGS_AUTO_SAVE: std::cell::RefCell<AutoSave> =
        const { std::cell::RefCell::new(AutoSave::new()) };
    // Mouse buttons currently held, so AutoSave never writes mid-drag
    // (same idle guard the native shell applies). Kept truthful by three
    // independent paths — see `mouse_idle`.
    static MOUSE_HELD: Cell<u32> = const { Cell::new(0) };
}

/// Seed the auto-saver with the blob already in storage, so the first
/// painted frame doesn't rewrite an identical value.
pub fn seed_auto_save(blob: String) {
    SETTINGS_AUTO_SAVE.with(|c| c.borrow_mut().seed(blob));
}

/// Number of mouse buttons currently pressed according to a
/// `MouseEvent.buttons` bitmask (bit 0 = left, 1 = right, 2 = middle).
///
/// Higher bits (back / forward / eraser) are deliberately ignored: they
/// never start a drag in this app, and counting them would keep the idle
/// guard closed for a button the widget tree doesn't track.
pub fn pressed_button_count(buttons: u32) -> u32 {
    (buttons & 0b111).count_ones()
}

/// Record a `mousedown`. Only ever raises the count; `mousemove` is what
/// corrects it (see [`sync_mouse_buttons`]).
pub fn note_mouse_down() {
    MOUSE_HELD.with(|c| c.set(c.get().saturating_add(1)));
}

/// Record a `mouseup` delivered to the canvas.
pub fn note_mouse_up() {
    MOUSE_HELD.with(|c| c.set(c.get().saturating_sub(1)));
}

/// Adopt the authoritative held-button count carried by a DOM mouse
/// event's `buttons` bitmask.
///
/// This is the self-healing path: `mousedown`/`mouseup` counting alone
/// desynchronises the moment a release happens where the canvas listener
/// can't see it (pointer dragged over browser chrome, another window
/// raised, a `mouseup` swallowed by a native context menu). Every
/// `mousemove` re-derives the truth from the event, so the guard
/// un-wedges on the very next cursor motion instead of staying closed
/// for the rest of the session.
pub fn sync_mouse_buttons(buttons: u32) {
    MOUSE_HELD.with(|c| c.set(pressed_button_count(buttons)));
}

/// True when no mouse button is held, i.e. it is safe to persist.
///
/// The count this reads is maintained by three paths, each of which is
/// sufficient on its own to reopen the guard after a lost `mouseup`:
/// canvas `mousedown`/`mouseup` counting, the `buttons` resync on every
/// `mousemove`, and the window-level release listener installed by
/// [`install_window_mouse_release_listener`].
pub fn mouse_idle() -> bool {
    MOUSE_HELD.with(|c| c.get()) == 0
}

/// Tick the diff-guarded auto-save.
///
/// `compose` is only called when the write is allowed, so idle frames
/// cost nothing. `force` bypasses the mouse-idle guard — used by the
/// page-hide flush, where a drag in progress is no reason to lose the
/// user's settings (the page is going away either way).
pub fn tick_auto_save(force: bool, compose: impl FnOnce() -> String, write: fn(&str)) {
    let allowed = force || mouse_idle();
    SETTINGS_AUTO_SAVE.with(|c| c.borrow_mut().tick(allowed, compose, write));
}

/// Register a window-level `pointerup` / `mouseup` listener that clears
/// the held-button count.
///
/// The canvas listeners in `index.html` see only releases that happen
/// over the canvas. A press that ends over the browser's own chrome — or
/// anywhere outside the document — never delivers a canvas `mouseup`,
/// which used to leave the idle guard permanently closed and settings
/// permanently unsaved. Listening on `window` catches every release that
/// reaches the page at all, and the `buttons` bitmask on the event tells
/// us whether *other* buttons are still down.
///
/// Both event names are registered because they are complementary rather
/// than redundant: `pointerup` also fires for touch and pen input, while
/// `mouseup` covers the (rare) case of a browser without Pointer Events.
/// A release seen twice is harmless — both handlers assign the same
/// derived count rather than decrementing.
pub fn install_window_mouse_release_listener() {
    let Some(window) = web_sys::window() else {
        return;
    };
    for event in ["pointerup", "mouseup", "pointercancel"] {
        let closure = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
            // `PointerEvent` extends `MouseEvent`, so one cast covers
            // both listeners. A cancel with no mouse data (or an event
            // shape we don't recognise) falls back to "nothing held",
            // which is the safe direction: worst case we persist a
            // frame early.
            let buttons = e
                .dyn_ref::<web_sys::MouseEvent>()
                .map(|m| m.buttons() as u32)
                .unwrap_or(0);
            sync_mouse_buttons(buttons);
        });
        if window
            .add_event_listener_with_callback(event, closure.as_ref().unchecked_ref())
            .is_ok()
        {
            // Leaked deliberately: these listeners live as long as the
            // page does, and there is no teardown path in a wasm shell
            // that would ever remove them.
            closure.forget();
        }
    }
}

/// Register the page-hide flush: persist settings when the document is
/// hidden or the page is being torn down.
///
/// `visibilitychange` → `document.hidden` and `pagehide` are the
/// *documented* teardown hooks for exactly this purpose, and they fire
/// in the cases `beforeunload`/`unload` famously miss — a mobile tab
/// backgrounded by the OS, or a page frozen into the back/forward cache.
/// Together they are as close to a reliable "last chance to write" as
/// the platform offers.
///
/// The flush bypasses the mouse-idle guard: a page hidden mid-drag would
/// otherwise silently drop everything changed since the drag began.
pub fn install_page_hide_flush(flush: impl Fn() + 'static) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let flush = std::rc::Rc::new(flush);

    if let Some(document) = window.document() {
        let f = flush.clone();
        let doc = document.clone();
        let closure = Closure::<dyn FnMut()>::new(move || {
            // Only the hidden transition matters; becoming visible again
            // has nothing to persist that the frame loop won't catch.
            if doc.hidden() {
                f();
            }
        });
        if document
            .add_event_listener_with_callback(
                "visibilitychange",
                closure.as_ref().unchecked_ref(),
            )
            .is_ok()
        {
            closure.forget();
        }
    }

    let f = flush.clone();
    let closure = Closure::<dyn FnMut()>::new(move || f());
    if window
        .add_event_listener_with_callback("pagehide", closure.as_ref().unchecked_ref())
        .is_ok()
    {
        closure.forget();
    }
}

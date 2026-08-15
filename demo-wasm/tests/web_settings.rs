//! Browser coverage for the WASM shell's settings persistence
//! (`demo-wasm::web_settings`) — the web counterpart of the native
//! shell's settings.txt round trip.
//!
//! `localStorage` is a browser API, so this file compiles out on native
//! and is **not** part of `cargo test --workspace`. Run it with a
//! headless Chrome, exactly like `atomartist-storage/tests/browser_opfs.rs`
//! (see `atomartist-storage/README.md` for the chromedriver-version
//! caveat):
//!
//! ```text
//! cargo install wasm-bindgen-cli --version 0.2.120
//! $env:CHROMEDRIVER = "C:\tools\chromedriver\chromedriver.exe"
//! $env:CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER = "wasm-bindgen-test-runner"
//! cargo test -p demo-wasm --target wasm32-unknown-unknown --test web_settings
//! ```
//!
//! The tests write the real `atomartist.settings` key, so each one saves
//! whatever was there and puts it back afterwards — running the suite in
//! a browser profile that also holds real settings is safe.

#![cfg(target_arch = "wasm32")]

use atomartist_renderer::RenderStyle;
use atomartist_storage::StorageUri;
use atomartist_ui::UiSettings;
use demo_wasm::web_settings::{
    load_settings, read_settings_blob, settings_from_stored, write_settings_blob, SETTINGS_KEY,
};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

fn storage() -> web_sys::Storage {
    web_sys::window()
        .expect("window")
        .local_storage()
        .expect("local_storage access")
        .expect("localStorage available in the test browser")
}

/// RAII restore of the settings key.
///
/// The restore lives in `Drop` rather than after the call so a *failing*
/// assertion still puts the developer's real settings back: a straight
/// `body(); restore();` skips the restore entirely on panic, which is
/// precisely the case where cleanup matters.
///
/// Caveat, stated rather than papered over: this depends on the panic
/// unwinding. `wasm-bindgen-test` on `wasm32-unknown-unknown` runs with
/// `panic = "abort"` semantics unless the toolchain's unwinding support
/// is enabled, in which case a panicking test aborts the module and no
/// destructor runs. The guard is still the right shape — it is correct
/// under unwinding and no worse than the old code under abort — but a
/// test that panics may leave the sample blob behind, and a browser
/// profile holding real settings should not be reused for a debugging
/// session on a failing test.
struct SettingsGuard {
    previous: Option<String>,
}

impl Drop for SettingsGuard {
    fn drop(&mut self) {
        let s = storage();
        match self.previous.take() {
            Some(blob) => s.set_item(SETTINGS_KEY, &blob).expect("restore"),
            None => s.remove_item(SETTINGS_KEY).expect("clear"),
        }
    }
}

/// Save the current value, run `body`, then restore it — so the suite
/// can't clobber a developer's real settings on the same origin.
fn with_saved_settings(body: impl FnOnce()) {
    let _guard = SettingsGuard {
        previous: read_settings_blob(),
    };
    body();
}

/// A settings value with every field moved off its default, so a
/// round-trip failure in any one of them shows up.
fn sample_settings() -> UiSettings {
    UiSettings {
        perspective: false,
        turntable: false,
        show_bed: false,
        render_style: RenderStyle::Overhang,
        snap_amount: 0.25,
        last_project_path: Some(
            "browser:///projects/bracket.atmr"
                .parse::<StorageUri>()
                .expect("valid uri"),
        ),
        recent_projects: vec![
            "browser:///projects/bracket.atmr".parse().unwrap(),
            "browser:///projects/gear.atmr".parse().unwrap(),
        ],
        ..UiSettings::default()
    }
}

#[wasm_bindgen_test]
fn settings_written_to_local_storage_are_read_back_unchanged() {
    with_saved_settings(|| {
        let settings = sample_settings();
        write_settings_blob(&settings.to_text());

        assert_eq!(
            load_settings(),
            settings,
            "a reload must see exactly what the last frame persisted"
        );
        // The last-project URI is the whole point of the feature: it is
        // what the startup auto-reopen reads.
        assert_eq!(
            load_settings().last_project_path,
            settings.last_project_path
        );
    });
}

#[wasm_bindgen_test]
fn the_blob_lands_under_the_documented_key() {
    with_saved_settings(|| {
        write_settings_blob("perspective=false\n");
        assert_eq!(
            storage().get_item(SETTINGS_KEY).expect("get_item"),
            Some("perspective=false\n".to_string()),
            "the key is part of the on-disk contract; changing it silently \
             drops every user's settings"
        );
    });
}

#[wasm_bindgen_test]
fn a_corrupted_stored_value_degrades_to_defaults() {
    with_saved_settings(|| {
        storage()
            .set_item(SETTINGS_KEY, "\u{0}not settings at all }{ ~~~")
            .expect("set corrupted value");
        assert_eq!(
            load_settings(),
            UiSettings::default(),
            "garbage in storage must never block startup"
        );
    });
}

#[wasm_bindgen_test]
fn an_absent_key_degrades_to_defaults() {
    with_saved_settings(|| {
        storage().remove_item(SETTINGS_KEY).expect("remove");
        assert_eq!(read_settings_blob(), None);
        assert_eq!(load_settings(), UiSettings::default());
    });
}

/// The helper's own contract: whatever the body did to the key, the
/// value present beforehand is what's there afterwards. (Only the
/// non-panicking path is exercised — see `SettingsGuard`'s note on why
/// the panicking one isn't reliably observable under wasm.)
#[wasm_bindgen_test]
fn the_helper_restores_the_previous_value() {
    let outer = "restore-me=1\n";
    storage().set_item(SETTINGS_KEY, outer).expect("seed");
    with_saved_settings(|| {
        write_settings_blob("clobbered=1\n");
        assert_eq!(read_settings_blob(), Some("clobbered=1\n".to_string()));
    });
    assert_eq!(read_settings_blob(), Some(outer.to_string()));

    // ...and an absent key is restored as absent, not as an empty string.
    storage().remove_item(SETTINGS_KEY).expect("clear");
    with_saved_settings(|| write_settings_blob("clobbered=1\n"));
    assert_eq!(read_settings_blob(), None);
}

/// Parsing is pure, so first-launch and corruption behave the same with
/// or without a browser involved.
#[wasm_bindgen_test]
fn settings_from_stored_is_forgiving() {
    assert_eq!(settings_from_stored(None), UiSettings::default());
    assert_eq!(settings_from_stored(Some("")), UiSettings::default());
    // A partially-valid blob keeps what it can and defaults the rest.
    let partial = settings_from_stored(Some("show_bed=false\nsnap_amount=not-a-number\n"));
    assert!(!partial.show_bed);
    assert_eq!(partial.snap_amount, UiSettings::default().snap_amount);
}

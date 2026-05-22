//! Top menu bar — File / Edit / Settings / Help / Add Node.
//!
//! Mirrors NodeDesigner's chrome. Actions are dispatched via string ids;
//! the parent app translates them into graph mutations / file dialogs.
//! For now the action strings are surfaced via the `on_action` callback
//! handed to `MenuBar`; future iterations wire them up to `AppState`.

use std::path::PathBuf;
use std::sync::Arc;

use agg_gui::{text::Font, MenuBar, MenuEntry, MenuItem, TopMenu, Widget};

use crate::app_state::AppState;
use crate::debug_windows::DebugWindowHandles;

mod bi {
    pub const ARROW_CLOCKWISE: char = '\u{f116}';
    pub const ARROW_COUNTERCLOCKWISE: char = '\u{f117}';
    pub const ARROWS_ANGLE_EXPAND: char = '\u{f14a}';
    pub const BOOK: char = '\u{f194}';
    pub const BOX: char = '\u{f1c8}';
    pub const BOX_ARROW_UP_RIGHT: char = '\u{f1c5}';
    pub const BOX_SEAM: char = '\u{f1c7}';
    pub const BUG: char = '\u{f2a3}';
    pub const CALCULATOR: char = '\u{f1e0}';
    pub const FILE_PLUS: char = '\u{f3ab}';
    pub const FLOPPY: char = '\u{f7d8}';
    pub const FOLDER2_OPEN: char = '\u{f3d8}';
    pub const INFO_CIRCLE: char = '\u{f431}';
    pub const PLUG: char = '\u{f4f7}';
    pub const PLUS_CIRCLE: char = '\u{f4fa}';
    pub const SPEEDOMETER: char = '\u{f55a}';
    pub const SUN: char = '\u{f5a2}';
    pub const TRASH: char = '\u{f5de}';
    pub const VECTOR_PEN: char = '\u{f604}';
}

/// Platform-supplied file-picker hooks. demo-native provides an `rfd`-
/// backed implementation; demo-wasm will provide a browser File API
/// version. The trait is invoked from the menu's action callback so the
/// platform can put up a modal dialog and return the chosen path.
pub trait FileDialogProvider: Send + Sync {
    fn pick_open_project(&self) -> Option<PathBuf>;
    fn pick_save_project(&self, default_name: &str) -> Option<PathBuf>;
    fn pick_save_stl(&self, default_name: &str) -> Option<PathBuf>;
    /// User-facing error notice — typically a message dialog. Returning
    /// nothing keeps the trait simple; severity is implicit "error".
    fn show_error(&self, message: &str);
    /// User-facing informational notice — used by License / About flows.
    fn show_info(&self, title: &str, message: &str);
}

/// No-op file-dialog provider used by tests / WASM (until Phase 10 wires
/// up the browser File API). Every call returns `None`.
pub struct NoFileDialogs;
impl FileDialogProvider for NoFileDialogs {
    fn pick_open_project(&self) -> Option<PathBuf> { None }
    fn pick_save_project(&self, _name: &str) -> Option<PathBuf> { None }
    fn pick_save_stl(&self, _name: &str) -> Option<PathBuf> { None }
    fn show_error(&self, _message: &str) {}
    fn show_info(&self, _title: &str, _message: &str) {}
}

/// Build the application's top menu bar widget. `state` is captured so
/// menu actions can mutate the graph (load/save, undo/redo, add-node).
/// `dialogs` injects platform-specific file pickers; pass
/// `NoFileDialogs` from tests / non-native shells. `debug` carries the
/// shared visibility cells so the `View → Debug` items can toggle the
/// Inspector / Performance windows.
pub fn build_menu_bar(
    state: AppState,
    font: Arc<Font>,
    dialogs: Arc<dyn FileDialogProvider>,
    debug: DebugWindowHandles,
) -> MenuBar {
    let menus = vec![
        TopMenu::new(
            "File",
            vec![
                MenuEntry::Item(MenuItem::action("New", "file.new").icon(bi::FILE_PLUS)),
                MenuEntry::Item(MenuItem::action("Open\u{2026}", "file.open").icon(bi::FOLDER2_OPEN)),
                MenuEntry::Separator,
                MenuEntry::Item(MenuItem::action("Save", "file.save").icon(bi::FLOPPY)),
                MenuEntry::Item(MenuItem::action("Save As\u{2026}", "file.save_as").icon(bi::FLOPPY)),
                MenuEntry::Separator,
                MenuEntry::Item(MenuItem::action("Export STL\u{2026}", "file.export_stl").icon(bi::BOX_ARROW_UP_RIGHT)),
            ],
        ),
        TopMenu::new(
            "Edit",
            vec![
                MenuEntry::Item(MenuItem::action("Undo", "edit.undo").icon(bi::ARROW_COUNTERCLOCKWISE)),
                MenuEntry::Item(MenuItem::action("Redo", "edit.redo").icon(bi::ARROW_CLOCKWISE)),
                MenuEntry::Separator,
                MenuEntry::Item(MenuItem::action("Delete Selected", "edit.delete").icon(bi::TRASH)),
                MenuEntry::Item(MenuItem::action("Select All", "edit.select_all").icon(bi::ARROWS_ANGLE_EXPAND)),
            ],
        ),
        TopMenu::new(
            "View",
            vec![MenuEntry::Item(
                MenuItem::submenu(
                    "Debug",
                    vec![
                        MenuEntry::Item(
                            MenuItem::action("Inspector", "view.debug.inspector").icon(bi::BUG),
                        ),
                        MenuEntry::Item(
                            MenuItem::action("Performance Graph", "view.debug.performance")
                                .icon(bi::SPEEDOMETER),
                        ),
                    ],
                )
                .icon(bi::BUG),
            )],
        ),
        TopMenu::new(
            "Settings",
            vec![
                MenuEntry::Item(MenuItem::action("Light Theme", "settings.theme.light").icon(bi::SUN)),
                MenuEntry::Item(MenuItem::action("Dark Theme", "settings.theme.dark").icon(bi::SUN)),
            ],
        ),
        TopMenu::new(
            "Help",
            vec![
                MenuEntry::Item(MenuItem::action("Documentation", "help.docs").icon(bi::BOOK)),
                MenuEntry::Item(MenuItem::action("License", "help.license").icon(bi::INFO_CIRCLE)),
                MenuEntry::Item(MenuItem::action("About", "help.about").icon(bi::INFO_CIRCLE)),
            ],
        ),
        // "Add Node" lists every registered node type, grouped by category.
        TopMenu::new("Add Node", build_add_node_entries(&state)),
    ];

    let dispatch_state = state;
    let dispatch_dialogs = dialogs;
    let dispatch_debug = debug;
    MenuBar::new(font, menus, move |action| {
        handle_action(
            &dispatch_state,
            dispatch_dialogs.as_ref(),
            &dispatch_debug,
            action,
        );
    })
    .with_font_size(13.0)
    // Tight width — lets the parent FlexRow place chrome on the right.
    .with_fit_width(true)
}

/// Backwards-compat name kept for callers that imported the SizedBox
/// wrapper. Now that agg-gui's MenuBar supports `fit_width` natively
/// the wrapper isn't needed; this just forwards to `build_menu_bar`.
pub fn build_menu_bar_sized(
    state: AppState,
    font: Arc<Font>,
    dialogs: Arc<dyn FileDialogProvider>,
    debug: DebugWindowHandles,
) -> Box<dyn Widget> {
    Box::new(build_menu_bar(state, font, dialogs, debug))
}

/// Walk the `NodeRegistry` and build a category-grouped Add Node submenu
/// list. Each leaf is a `MenuItem` whose action is `"add.{type_id}"`.
fn build_add_node_entries(state: &AppState) -> Vec<MenuEntry> {
    let mut out = Vec::new();
    for (cat, defs) in state.registry.by_category() {
        if defs.is_empty() {
            continue;
        }
        let items = defs
            .iter()
            .map(|d| {
                MenuEntry::Item(MenuItem::action(
                    d.display_name(),
                    format!("add.{}", d.type_id()),
                ).icon(bi::PLUS_CIRCLE))
            })
            .collect();
        let submenu = match category_icon(cat) {
            Some(icon) => MenuItem::submenu(cat, items).icon(icon),
            None => MenuItem::submenu(cat, items),
        };
        out.push(MenuEntry::Item(submenu));
    }
    out
}

fn category_icon(category: &str) -> Option<char> {
    match category {
        "Primitives 2D" | "Operations 2D" => Some(bi::VECTOR_PEN),
        "Primitives 3D" => Some(bi::BOX),
        "Operations 3D" => Some(bi::ARROWS_ANGLE_EXPAND),
        "Mesh" => Some(bi::BOX_SEAM),
        "Math" => Some(bi::CALCULATOR),
        "Output" => Some(bi::PLUG),
        _ => None,
    }
}

fn handle_action(
    state: &AppState,
    dialogs: &dyn FileDialogProvider,
    debug: &DebugWindowHandles,
    action: &str,
) {
    use agg_gui::theme::{set_visuals, Visuals};
    if let Some(type_id) = action.strip_prefix("add.") {
        // Find the action's NodeDef by its dynamic type_id string and
        // intern it. Registry stores &'static str ids; we look up the
        // exact one rather than leaking new memory each call.
        let interned = state
            .registry
            .iter()
            .map(|d| d.type_id())
            .find(|s| *s == type_id);
        if let Some(static_id) = interned {
            let mut g = state.graph.lock().unwrap();
            let _ = crate::node_helpers::add_node_with_defaults(
                &mut g,
                &state.registry,
                static_id,
                [80.0, 220.0],
            );
            drop(g);
            state.schedule_evaluate();
        }
        return;
    }
    match action {
        "settings.theme.light" => set_visuals(Visuals::light()),
        "settings.theme.dark" => set_visuals(Visuals::dark()),
        "edit.undo" => {
            let mut buf = state.undo.lock().unwrap();
            buf.undo();
            state.schedule_evaluate();
        }
        "edit.redo" => {
            let mut buf = state.undo.lock().unwrap();
            buf.redo();
            state.schedule_evaluate();
        }
        "file.new" => state.new_empty_project(),
        "file.open" => {
            if let Some(path) = dialogs.pick_open_project() {
                if let Err(e) = state.load_graph_from_path(&path) {
                    dialogs.show_error(&format!("Open failed: {}", e));
                }
            }
        }
        "file.save" => {
            // If we already have a path, save directly. Otherwise prompt.
            let existing = state.current_file.lock().unwrap().clone();
            let path = match existing {
                Some(p) => Some(p),
                None => dialogs.pick_save_project("untitled.atomartist.json"),
            };
            if let Some(p) = path {
                if let Err(e) = state.save_graph_to_path(&p) {
                    dialogs.show_error(&format!("Save failed: {}", e));
                }
            }
        }
        "file.save_as" => {
            let suggested = state
                .current_file
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "untitled.atomartist.json".to_string());
            if let Some(path) = dialogs.pick_save_project(&suggested) {
                if let Err(e) = state.save_graph_to_path(&path) {
                    dialogs.show_error(&format!("Save failed: {}", e));
                }
            }
        }
        "file.export_stl" => {
            if let Some(path) = dialogs.pick_save_stl("export.stl") {
                if let Err(e) = state.export_stl_to_path(&path) {
                    dialogs.show_error(&format!("Export failed: {}", e));
                }
            }
        }
        "help.about" => {
            dialogs.show_info(
                "About AtomArtist",
                &format!(
                    "AtomArtist v{}\n\n\
                    A pure-Rust visual node-based 3D design tool.\n\
                    Built on agg-gui + manifold-rust + clipper2-rust + tess2-rust.\n\n\
                    https://github.com/larsbrubaker/atomartist",
                    env!("CARGO_PKG_VERSION"),
                ),
            );
        }
        "help.license" => {
            dialogs.show_info(
                "License",
                "AtomArtist is licensed under the MIT License.\n\
                See the LICENSE file in the project root for the full text.",
            );
        }
        "help.docs" => {
            dialogs.show_info(
                "Documentation",
                "Documentation lives in README.md and CLAUDE.md\n\
                in the project repository.\n\n\
                https://github.com/larsbrubaker/atomartist",
            );
        }
        "view.debug.inspector" => {
            debug.inspector_visible.set(!debug.inspector_visible.get());
        }
        "view.debug.performance" => {
            debug.perf_visible.set(!debug.perf_visible.get());
        }
        _ => {}
    }
}

//! Top menu bar — File / Edit / Settings / Help / Add Node.
//!
//! Mirrors NodeDesigner's chrome. Actions are dispatched via string ids;
//! the parent app translates them into graph mutations / file dialogs.
//! For now the action strings are surfaced via the `on_action` callback
//! handed to `MenuBar`; future iterations wire them up to `AppState`.

use std::path::PathBuf;
use std::sync::Arc;

use agg_gui::{text::Font, MenuBar, MenuEntry, MenuItem, TopMenu};

use crate::app_state::AppState;

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
}

/// No-op file-dialog provider used by tests / WASM (until Phase 10 wires
/// up the browser File API). Every call returns `None`.
pub struct NoFileDialogs;
impl FileDialogProvider for NoFileDialogs {
    fn pick_open_project(&self) -> Option<PathBuf> { None }
    fn pick_save_project(&self, _name: &str) -> Option<PathBuf> { None }
    fn pick_save_stl(&self, _name: &str) -> Option<PathBuf> { None }
    fn show_error(&self, _message: &str) {}
}

/// Build the application's top menu bar widget. `state` is captured so
/// menu actions can mutate the graph (load/save, undo/redo, add-node).
/// `dialogs` injects platform-specific file pickers; pass
/// `NoFileDialogs` from tests / non-native shells.
pub fn build_menu_bar(
    state: AppState,
    font: Arc<Font>,
    dialogs: Arc<dyn FileDialogProvider>,
) -> MenuBar {
    let menus = vec![
        TopMenu::new(
            "File",
            vec![
                MenuEntry::Item(MenuItem::action("New", "file.new")),
                MenuEntry::Item(MenuItem::action("Open\u{2026}", "file.open")),
                MenuEntry::Separator,
                MenuEntry::Item(MenuItem::action("Save", "file.save")),
                MenuEntry::Item(MenuItem::action("Save As\u{2026}", "file.save_as")),
                MenuEntry::Separator,
                MenuEntry::Item(MenuItem::action("Export STL\u{2026}", "file.export_stl")),
            ],
        ),
        TopMenu::new(
            "Edit",
            vec![
                MenuEntry::Item(MenuItem::action("Undo", "edit.undo")),
                MenuEntry::Item(MenuItem::action("Redo", "edit.redo")),
                MenuEntry::Separator,
                MenuEntry::Item(MenuItem::action("Delete Selected", "edit.delete")),
                MenuEntry::Item(MenuItem::action("Select All", "edit.select_all")),
            ],
        ),
        TopMenu::new(
            "Settings",
            vec![
                MenuEntry::Item(MenuItem::action("Light Theme", "settings.theme.light")),
                MenuEntry::Item(MenuItem::action("Dark Theme", "settings.theme.dark")),
            ],
        ),
        TopMenu::new(
            "Help",
            vec![
                MenuEntry::Item(MenuItem::action("Documentation", "help.docs")),
                MenuEntry::Item(MenuItem::action("About", "help.about")),
            ],
        ),
        // "Add Node" lists every registered node type, grouped by category.
        TopMenu::new("Add Node", build_add_node_entries(&state)),
    ];

    let dispatch_state = state;
    let dispatch_dialogs = dialogs;
    MenuBar::new(font, menus, move |action| {
        handle_action(&dispatch_state, dispatch_dialogs.as_ref(), action);
    })
    .with_font_size(13.0)
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
                ))
            })
            .collect();
        out.push(MenuEntry::Item(MenuItem::submenu(cat, items)));
    }
    out
}

fn handle_action(state: &AppState, dialogs: &dyn FileDialogProvider, action: &str) {
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
            let _ = crate::canvas_widget::add_node_with_defaults(
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
        _ => {}
    }
}

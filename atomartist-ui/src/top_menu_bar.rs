//! Top menu bar — File / Edit / View / Help / Add Node.
//!
//! Mirrors NodeDesigner's chrome. Actions are dispatched via string ids
//! routed through `menu_actions::handle_action`. The bar itself is
//! hosted inside [`MenuChrome`], a thin wrapper that rebuilds the menu
//! list whenever state it renders (theme / accent radios, the recent-
//! projects list) changes — `MenuItem::radio` marks are baked in at
//! construction, so a static bar would go stale after the first change.

use std::sync::Arc;

use atomartist_storage::{Job, StorageUri};

use agg_gui::{
    text::Font,
    theme::{AccentColor, ThemePreference},
    widget::{BackbufferCache, BackbufferMode},
    DrawCtx, Event, EventResult, Key, MenuBar, MenuEntry, MenuItem, Modifiers,
    Rect, Size, TopMenu, Widget,
};

use crate::app_state::AppState;
use crate::debug_windows::DebugWindowHandles;
use crate::fa;
use crate::menu_actions::handle_action;

/// User's answer to the "you have unsaved changes" prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsavedChoice {
    /// Save first, then proceed with the destructive action.
    Save,
    /// Throw the changes away and proceed.
    Discard,
    /// Abort the action entirely.
    Cancel,
}

/// Platform-supplied file-picker hooks. demo-native provides an `rfd`-
/// backed implementation; demo-wasm (and any shell with non-`file:`
/// providers) uses
/// [`ModalFileDialogs`](crate::file_browser::ModalFileDialogs), which
/// routes every pick through the in-app file browser. The trait is
/// invoked from the menu's action callback so the platform can put a
/// picker up and answer with the chosen location.
///
/// Pickers answer with a [`StorageUri`], not a path: a native shell
/// converts its `PathBuf` with `StorageUri::from_local_path`, while a
/// cloud-backed picker names an object in its own scheme.
///
/// # Why the pickers return a `Job`
///
/// The in-app browser is a widget: it cannot answer before the frame loop
/// has run, so a blocking signature is impossible for it (step 6c-2 of
/// `docs/file-browser-design.md`). Every picker therefore hands back a
/// [`Job<Option<StorageUri>>`] that settles
/// `Ok(Some(uri))` on confirm and `Ok(None)` on cancel;
/// [`crate::menu_actions`] wraps it in a [`crate::storage_ops::JobOp`] so
/// the continuation runs from the frame pump. A blocking implementation
/// (`rfd`) simply returns [`Job::ready`], which `submit_op` applies inline
/// — the desktop path stays exactly as immediate as it was.
///
/// A failed job is *not* a cancellation: the modal handle fails a stacked
/// open with [`atomartist_storage::StorageError::Cancelled`], and
/// `menu_actions` reports anything else as a notice.
///
/// # Still blocking, deliberately
///
/// [`confirm_unsaved_changes`](Self::confirm_unsaved_changes),
/// [`show_error`](Self::show_error), and [`show_info`](Self::show_info)
/// keep their synchronous shapes. A generic in-app confirm/notice modal is
/// future work (design §5, 6d); until it exists, implementations without a
/// native message box answer `Cancel` (recoverable) and route messages to
/// the status-bar notice queue.
pub trait FileDialogProvider: Send + Sync {
    fn pick_open_project(&self) -> Job<Option<StorageUri>>;
    fn pick_save_project(&self, default_name: &str) -> Job<Option<StorageUri>>;
    /// Destination picker for File → Export. `extension` is the
    /// lowercase format extension without the dot ("stl", "3mf",
    /// "obj", "atmr"); implementations use it for the dialog filter and
    /// for the extension a typed name is forced to.
    fn pick_save_export(&self, extension: &str, default_name: &str)
        -> Job<Option<StorageUri>>;
    /// Source picker for File → Import — meshes (`.stl` / `.obj` /
    /// `.3mf`), MatterControl scenes (`.mcx`), and AtomArtist projects
    /// (`.atmr`).
    fn pick_import_file(&self) -> Job<Option<StorageUri>>;
    /// "You have unsaved changes" — Save / Discard / Cancel. Shown
    /// before New / Open / recent-open and by the shell before close.
    fn confirm_unsaved_changes(&self) -> UnsavedChoice;
    /// User-facing error notice — typically a message dialog. Returning
    /// nothing keeps the trait simple; severity is implicit "error".
    fn show_error(&self, message: &str);
    /// User-facing informational notice — used by License / About flows.
    fn show_info(&self, title: &str, message: &str);
}

/// No-op file-dialog provider used by tests and by any shell with no
/// picker at all. Every picker settles `None` immediately — the same
/// answer a cancelled dialog gives — and the unsaved-changes prompt
/// answers `Discard` so scripted flows never block.
pub struct NoFileDialogs;
impl FileDialogProvider for NoFileDialogs {
    fn pick_open_project(&self) -> Job<Option<StorageUri>> { Job::ready(None) }
    fn pick_save_project(&self, _name: &str) -> Job<Option<StorageUri>> { Job::ready(None) }
    fn pick_save_export(&self, _ext: &str, _name: &str) -> Job<Option<StorageUri>> {
        Job::ready(None)
    }
    fn pick_import_file(&self) -> Job<Option<StorageUri>> { Job::ready(None) }
    fn confirm_unsaved_changes(&self) -> UnsavedChoice { UnsavedChoice::Discard }
    fn show_error(&self, _message: &str) {}
    fn show_info(&self, _title: &str, _message: &str) {}
}

/// Compose the full menu list from the current app state. Called at
/// construction and again by [`MenuChrome`] whenever the state the
/// menus render has changed.
fn compose_menus(state: &AppState) -> Vec<TopMenu> {
    vec![
        TopMenu::new("File", build_file_entries(state)),
        TopMenu::new(
            "Edit",
            vec![
                MenuEntry::Item(
                    MenuItem::action("Undo", "edit.undo")
                        .icon(fa::UNDO)
                        .shortcut("Ctrl+Z"),
                ),
                MenuEntry::Item(
                    MenuItem::action("Redo", "edit.redo")
                        .icon(fa::REDO)
                        .shortcut("Ctrl+Y"),
                ),
                MenuEntry::Separator,
                MenuEntry::Item(
                    MenuItem::action("Delete Selected", "edit.delete")
                        .icon(fa::TRASH)
                        .shortcut("Del"),
                ),
                MenuEntry::Item(
                    MenuItem::action("Select All", "edit.select_all")
                        .icon(fa::EXPAND)
                        .shortcut("Ctrl+A"),
                ),
            ],
        ),
        TopMenu::new("View", build_view_entries(state)),
        TopMenu::new(
            "Help",
            vec![
                MenuEntry::Item(MenuItem::action("Documentation", "help.docs").icon(fa::BOOK)),
                MenuEntry::Item(MenuItem::action("License", "help.license").icon(fa::INFO_CIRCLE)),
                MenuEntry::Item(MenuItem::action("About", "help.about").icon(fa::INFO_CIRCLE)),
            ],
        ),
        // "Add Node" lists every registered node type, grouped by category.
        TopMenu::new("Add Node", build_add_node_entries(state)),
    ]
}

/// File menu: project lifecycle up top, then import/export.
fn build_file_entries(state: &AppState) -> Vec<MenuEntry> {
    let recent = state.recent_projects.lock().unwrap().clone();
    let recent_submenu: Vec<MenuEntry> = if recent.is_empty() {
        vec![MenuEntry::Item(
            MenuItem::action("(No Recent Projects)", "file.recent.none").disabled(),
        )]
    } else {
        recent
            .iter()
            .enumerate()
            .map(|(i, uri)| {
                // Last URI segment reads like a file name for every
                // scheme; the full URI is the fallback for a project
                // that sits at a provider root.
                let label = uri
                    .file_name()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| uri.to_string());
                MenuEntry::Item(MenuItem::action(label, format!("file.recent.{i}")))
            })
            .collect()
    };

    let export_submenu = vec![
        MenuEntry::Item(MenuItem::action("STL\u{2026}", "file.export.stl")),
        MenuEntry::Item(MenuItem::action("3MF\u{2026}", "file.export.3mf")),
        MenuEntry::Item(MenuItem::action("OBJ\u{2026}", "file.export.obj")),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::action(
            "AtomArtist Project\u{2026}",
            "file.export.atmr",
        )),
    ];

    vec![
        MenuEntry::Item(MenuItem::action("New", "file.new").icon(fa::FILE_NEW)),
        MenuEntry::Item(MenuItem::action("Open\u{2026}", "file.open").icon(fa::FOLDER_OPEN)),
        MenuEntry::Item(MenuItem::submenu("Open Recent", recent_submenu).icon(fa::FOLDER_OPEN)),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::action("Save", "file.save").icon(fa::SAVE)),
        MenuEntry::Item(MenuItem::action("Save As\u{2026}", "file.save_as").icon(fa::SAVE)),
        MenuEntry::Separator,
        MenuEntry::Item(
            MenuItem::action("Import\u{2026}", "file.import").icon(fa::IMPORT),
        ),
        MenuEntry::Item(MenuItem::submenu("Export", export_submenu).icon(fa::EXPORT)),
    ]
}

/// Build the View menu — debug toggles, theme (Light / Dark), and an
/// AccentColor swatch picker. Mirrors the agg-gui demo's View menu so
/// the theme + accent affordances feel the same across both apps.
fn build_view_entries(state: &AppState) -> Vec<MenuEntry> {
    let theme = *state.theme.lock().unwrap();
    let accent = *state.accent_color.lock().unwrap();

    let theme_submenu = vec![
        MenuEntry::Item(
            MenuItem::action("Light", "view.theme.light")
                .icon(fa::SUN)
                .radio(theme == ThemePreference::Light)
                .keep_open(),
        ),
        MenuEntry::Item(
            MenuItem::action("Dark", "view.theme.dark")
                .icon(fa::SUN)
                .radio(theme == ThemePreference::Dark)
                .keep_open(),
        ),
        MenuEntry::Item(
            MenuItem::action("System", "view.theme.system")
                .icon(fa::SUN)
                .radio(theme == ThemePreference::System)
                .keep_open(),
        ),
    ];

    let accent_submenu: Vec<MenuEntry> = AccentColor::ALL
        .iter()
        .map(|a| {
            MenuEntry::Item(
                MenuItem::action(a.label(), format!("view.accent.{}", a.key()))
                    .swatch(a.color())
                    .radio(accent == *a)
                    .keep_open(),
            )
        })
        .collect();

    vec![
        MenuEntry::Item(
            MenuItem::submenu(
                "Debug",
                vec![
                    MenuEntry::Item(
                        MenuItem::action("Inspector", "view.debug.inspector").icon(fa::BUG),
                    ),
                    MenuEntry::Item(
                        MenuItem::action("Performance Graph", "view.debug.performance")
                            .icon(fa::TACHOMETER),
                    ),
                ],
            )
            .icon(fa::BUG),
        ),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::submenu("Theme", theme_submenu).icon(fa::SUN)),
        MenuEntry::Item(MenuItem::submenu("Color", accent_submenu).icon(fa::SUN)),
    ]
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
                ).icon(fa::PLUS_CIRCLE))
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
        "Primitives 2D" | "Operations 2D" => Some(fa::PENCIL),
        "Primitives 3D" => Some(fa::CUBE),
        "Operations 3D" => Some(fa::EXPAND),
        "Mesh" => Some(fa::CUBES),
        "Math" => Some(fa::CALCULATOR),
        "Input" => Some(fa::SLIDERS),
        "Output" => Some(fa::PLUG),
        _ => None,
    }
}

/// Wraps the `MenuBar` so its item tree can be regenerated from app
/// state. `MenuItem::radio` / the recent-files list are baked into the
/// items at construction; this wrapper diffs a snapshot of that state
/// in `layout` and swaps the menu list via [`MenuBar::set_menus`] when
/// it moves — same pattern as agg-gui's demo `MenuChrome`.
///
/// The bar is a concrete field (not a tree child) so we can call
/// `set_menus`; in exchange the wrapper forwards every Widget hook the
/// bar relies on.
pub struct MenuChrome {
    bounds: Rect,
    children: Vec<Box<dyn Widget>>, // intentionally empty — see struct docs
    bar: MenuBar,
    state: AppState,
    last_snapshot: Option<(ThemePreference, AccentColor, Vec<StorageUri>)>,
}

impl MenuChrome {
    fn snapshot(&self) -> (ThemePreference, AccentColor, Vec<StorageUri>) {
        (
            *self.state.theme.lock().unwrap(),
            *self.state.accent_color.lock().unwrap(),
            self.state.recent_projects.lock().unwrap().clone(),
        )
    }

    /// Rebuild the menu list if any rendered state changed since the
    /// last rebuild. Cheap when nothing changed.
    fn refresh_menus(&mut self) {
        let snapshot = self.snapshot();
        if self.last_snapshot.as_ref() == Some(&snapshot) {
            return;
        }
        self.bar.set_menus(compose_menus(&self.state));
        self.last_snapshot = Some(snapshot);
    }

    /// Read-only view of the composed menu list — test hook, mirrors
    /// [`MenuBar::menus`].
    pub fn menus(&self) -> &[TopMenu] {
        self.bar.menus()
    }
}

impl Widget for MenuChrome {
    fn type_name(&self) -> &'static str {
        "MenuChrome"
    }
    fn bounds(&self) -> Rect {
        self.bounds
    }
    fn set_bounds(&mut self, b: Rect) {
        self.bounds = b;
        self.bar.set_bounds(Rect::new(0.0, 0.0, b.width, b.height));
    }
    fn children(&self) -> &[Box<dyn Widget>] {
        &self.children
    }
    fn children_mut(&mut self) -> &mut Vec<Box<dyn Widget>> {
        &mut self.children
    }
    fn layout(&mut self, available: Size) -> Size {
        self.refresh_menus();
        let used = self.bar.layout(available);
        self.bounds = Rect::new(0.0, 0.0, used.width, used.height);
        self.bar
            .set_bounds(Rect::new(0.0, 0.0, used.width, used.height));
        used
    }
    fn paint(&mut self, ctx: &mut dyn DrawCtx) {
        self.bar.paint(ctx);
    }
    fn paint_global_overlay(&mut self, ctx: &mut dyn DrawCtx) {
        self.bar.paint_global_overlay(ctx);
    }
    fn hit_test_global_overlay(&self, local_pos: agg_gui::Point) -> bool {
        self.bar.hit_test_global_overlay(local_pos)
    }
    fn has_active_modal(&self) -> bool {
        self.bar.has_active_modal()
    }
    fn on_event(&mut self, event: &Event) -> EventResult {
        self.bar.on_event(event)
    }
    fn on_unconsumed_key(&mut self, key: &Key, modifiers: Modifiers) -> EventResult {
        self.bar.on_unconsumed_key(key, modifiers)
    }
    fn backbuffer_cache_mut(&mut self) -> Option<&mut BackbufferCache> {
        self.bar.backbuffer_cache_mut()
    }
    fn backbuffer_mode(&self) -> BackbufferMode {
        self.bar.backbuffer_mode()
    }
}

/// Build the application's top menu bar widget. `state` is captured so
/// menu actions can mutate the graph (load/save, undo/redo, add-node)
/// and so the chrome can rebuild the menus when rendered state (theme,
/// accent, recent files) changes. `dialogs` injects platform-specific
/// file pickers; pass `NoFileDialogs` from tests / non-native shells.
/// `debug` carries the shared visibility cells so the `View → Debug`
/// items can toggle the Inspector / Performance windows.
pub fn build_menu_bar(
    state: AppState,
    font: Arc<Font>,
    dialogs: Arc<dyn FileDialogProvider>,
    debug: DebugWindowHandles,
) -> MenuChrome {
    let menus = compose_menus(&state);
    let dispatch_state = state.clone();
    let dispatch_dialogs = dialogs;
    let dispatch_debug = debug;
    let bar = MenuBar::new(font, menus, move |action| {
        handle_action(&dispatch_state, &dispatch_dialogs, &dispatch_debug, action);
        agg_gui::animation::request_draw();
    })
    .with_font_size(13.0)
    // Tight width — lets the parent FlexRow place chrome on the right.
    .with_fit_width(true);
    MenuChrome {
        bounds: Rect::default(),
        children: Vec::new(),
        bar,
        state,
        last_snapshot: None,
    }
}

/// Boxed variant used by `top_level::build_app`.
pub fn build_menu_bar_sized(
    state: AppState,
    font: Arc<Font>,
    dialogs: Arc<dyn FileDialogProvider>,
    debug: DebugWindowHandles,
) -> Box<dyn Widget> {
    Box::new(build_menu_bar(state, font, dialogs, debug))
}

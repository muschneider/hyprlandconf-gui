// SPDX-License-Identifier: MIT OR Apache-2.0
//! `hyprconf-gui` — the Iced desktop front-end for hyprconf.
//!
//! This step wires `hyprconf-core` into the UI: on launch it locates and parses
//! the user's Hyprland config off the UI thread (via an `iced::Task`), then lets
//! the user browse and edit it. It also detects a running Hyprland (via
//! `hyprctl`) for optional live-apply/reload, keeps an undo/redo history, and
//! persists window/theme/format/recent-file settings between runs.

mod color_picker;
mod diff;
mod edit;
mod fuzzy;
mod load;
mod migrate;
mod profiles;
mod save;
mod settings;
mod view;

#[cfg(test)]
mod ui_tests;

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::{Element, Size, Task, Theme};

/// The Wayland `app_id` / X11 `WM_CLASS` the window announces itself under.
///
/// This is what a compositor or dock uses to pair the running window with an
/// installed `.desktop` entry — and therefore with an icon. It must stay equal
/// to the basename of `packaging/hyprconf.desktop` and to the icon name
/// installed under `share/icons/hicolor/*/apps/`; if the three drift apart the
/// window simply renders with the generic fallback icon.
const APP_ID: &str = "hyprconf";

use hyprconf_core::hyprctl::HyprlandInfo;
use hyprconf_core::outputs::DetectedMonitor;
use hyprconf_core::schema::{CollectionId, Schema};
use hyprconf_core::structured::MonitorRule;
use hyprconf_core::value::Color;
use hyprconf_core::{ConfigFormat, Value};

use crate::color_picker::{ColorDraft, ColorTarget};
use crate::edit::{EditAction, EditSnapshot, MonitorEdit};
use crate::load::{LoadState, Loaded};
use crate::migrate::{Migration, Step as MigrateStep};
use crate::settings::Settings;

fn main() -> anyhow::Result<()> {
    init_tracing();

    let args = parse_args();
    tracing::info!(
        gui_version = env!("CARGO_PKG_VERSION"),
        core_version = hyprconf_core::version(),
        config = ?args.config,
        check = args.check,
        "starting hyprconf",
    );

    // Headless sanity check: load and report, without opening a window.
    if args.check {
        return run_check(args.config);
    }

    let settings = Settings::load();
    let size = Size::new(settings.window_width, settings.window_height);
    let explicit = args.config;

    let migrate = args.migrate;

    iced::application(
        move || App::boot(explicit.clone(), settings.clone(), migrate),
        App::update,
        App::view,
    )
    .title(App::title)
    .theme(App::theme)
    .subscription(App::subscription)
    .window(window_settings(size))
    .run()?;

    Ok(())
}

/// The window configuration: restored size plus the identity the desktop needs
/// to find our icon.
///
/// Note there is deliberately no [`iced::window::Settings::icon`] here. Wayland
/// has no per-window icon in the core protocol, so a pixel buffer handed to
/// winit would be silently dropped on exactly the compositor this app targets.
/// The icon is instead resolved the way the desktop expects: `app_id` ->
/// `hyprconf.desktop` -> `Icon=hyprconf` -> `hicolor/*/apps/hyprconf.png`.
fn window_settings(size: Size) -> iced::window::Settings {
    iced::window::Settings {
        size,
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: APP_ID.to_owned(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Parsed command-line arguments.
#[derive(Debug, Default)]
struct Args {
    config: Option<PathBuf>,
    check: bool,
    migrate: bool,
}

/// Parse `--config <path>` / `--config=<path>`, `--check` and `--migrate`.
fn parse_args() -> Args {
    let mut parsed = Args::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--config=") {
            parsed.config = Some(PathBuf::from(value));
        } else if arg == "--config" {
            parsed.config = args.next().map(PathBuf::from);
        } else if arg == "--check" {
            parsed.check = true;
        } else if arg == "--migrate" {
            parsed.migrate = true;
        }
    }
    parsed
}

/// Load the config and print a one-line summary; used by `--check` (no window).
fn run_check(explicit: Option<PathBuf>) -> anyhow::Result<()> {
    match load::load_config(explicit) {
        LoadState::Loaded(loaded) => {
            // Lua code kept verbatim (loops, helpers) is not a problem; only
            // the rest are warnings.
            let code = loaded.dynamic_regions;
            println!(
                "loaded {} config: {} ({} options set, {} warnings, {} Lua code block(s) kept as-is, {} included file(s))",
                load::format_label(loaded.format),
                loaded.source.display(),
                loaded.config.option_count(),
                loaded.diagnostics.len().saturating_sub(code),
                code,
                loaded.included_files,
            );
            Ok(())
        }
        LoadState::NotFound { searched } => {
            let searched = searched
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("no configuration found (searched: {searched})")
        }
        LoadState::Error { path, message } => {
            anyhow::bail!("failed to load {}: {message}", path.display())
        }
        LoadState::Loading => Ok(()),
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).try_init();
}

/// Which sidebar entry is selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Selection {
    /// A schema section, by id.
    Section(String),
    /// A structured collection.
    Collection(CollectionId),
}

/// Which subset of a section's options a pane shows.
///
/// A single section can hold sixty options, the vast majority of them left at
/// their default. Filtering is what turns that wall into something you can
/// actually work in — "what did I change?" and "what is even set?" are the two
/// questions a config editor gets asked constantly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OptionFilter {
    /// Every option in the section.
    #[default]
    All,
    /// Only options the configuration file actually sets.
    Set,
    /// Only options with unsaved edits.
    Modified,
}

impl OptionFilter {
    /// The filters offered, in display order.
    pub(crate) const ALL: [OptionFilter; 3] =
        [OptionFilter::All, OptionFilter::Set, OptionFilter::Modified];

    /// The chip label.
    pub(crate) fn label(self) -> &'static str {
        match self {
            OptionFilter::All => "all",
            OptionFilter::Set => "set",
            OptionFilter::Modified => "modified",
        }
    }

    /// Whether an option passes this filter.
    pub(crate) fn keeps(self, loaded: &Loaded, path: &str) -> bool {
        match self {
            OptionFilter::All => true,
            OptionFilter::Set => loaded.config.get(path).is_some(),
            OptionFilter::Modified => loaded.is_dirty(path),
        }
    }
}

/// Messages produced by the UI and async tasks.
#[derive(Debug, Clone)]
pub(crate) enum Message {
    /// A theme was chosen from the picker.
    ThemeSelected(Theme),
    /// The background load finished.
    Loaded(Arc<LoadState>),
    /// A sidebar entry was selected.
    Selected(Selection),
    /// The search query changed.
    SearchChanged(String),
    /// An option was edited.
    Edit(edit::EditAction),
    /// A structured collection was edited.
    CollectionEdit(edit::CollectionAction),
    /// Undo the last edit.
    Undo,
    /// Redo the last undone edit.
    Redo,
    /// Toggle the pending-changes (diff) view.
    ToggleChanges,
    /// Open/close the save panel.
    ToggleSave,
    /// Open/close the diagnostics panel (issues found while loading).
    ToggleDiagnostics,
    /// Choose the output format in the save panel.
    SetOutputFormat(ConfigFormat),
    /// Toggle "save despite warnings".
    ToggleOverride(bool),
    /// Write the current plan to disk.
    PerformSave,
    /// A background Hyprland detection finished.
    HyprlandDetected(Option<HyprlandInfo>),
    /// A background output detection finished (`hyprctl monitors all -j`).
    MonitorsDetected(Arc<Result<Vec<DetectedMonitor>, String>>),
    /// Re-read the attached displays.
    RefreshMonitors,
    /// Edit the rule governing a physically attached output, by connector name.
    /// The rule is created (seeded from the display's current state) if the
    /// config doesn't mention it yet.
    MonitorEdit(String, MonitorEdit),
    /// A monitor was dropped after being dragged in the layout view.
    MonitorDropped(String),
    /// Show/hide a monitor card's advanced fields, by connector name.
    ToggleMonitorAdvanced(String),
    /// Toggle live-apply (push committed scalar edits via `hyprctl keyword`).
    ToggleLiveApply(bool),
    /// Ask the running Hyprland to reload its config.
    Reload,
    /// The result of a `hyprctl` invocation (apply/reload).
    HyprResult(Result<String, String>),
    /// The window was resized (persisted for next launch, throttled).
    WindowResized(f32, f32),
    /// Open/close the profiles & recent-files panel.
    ToggleProfiles,
    /// The profile-name field changed.
    ProfileNameChanged(String),
    /// Save the current config as a named profile.
    SaveProfile,
    /// The import-path field changed.
    ImportPathChanged(String),
    /// Open a config from an explicit path (recent / profile / import).
    OpenPath(PathBuf),
    /// Open the visual color picker for a scalar color option.
    OpenColorPicker(String),
    /// Open the visual color picker for a gradient stop (path, stop index).
    OpenStopColorPicker(String, usize),
    /// Close the color picker.
    CloseColorPicker,
    /// The 2D area reported a new saturation/value.
    PickSatVal(f32, f32),
    /// The hue strip reported a new hue (degrees).
    PickHue(f32),

    // -- `.conf` -> Lua migration ---------------------------------------
    /// Open the guided migration flow.
    StartMigration,
    /// Move to a specific step of the flow.
    MigrateGoto(MigrateStep),
    /// Hand the generated Lua to `Hyprland --verify-config`.
    MigrateCheck,
    /// The verification finished.
    MigrateChecked(Box<hyprconf_core::verify::Verdict>),
    /// Proceed even though the check did not pass (or could not run).
    MigrateOverride(bool),
    /// Write the migrated config to disk.
    MigrateApply,
    /// The write finished.
    MigrateApplied(Box<Result<migrate::Applied, String>>),
    /// Leave the migration flow.
    CloseMigration,
    /// Hide the deprecation banner for this session.
    DismissDeprecation,
    /// Move keyboard focus to the search field (Ctrl+F).
    FocusSearch,
    /// Narrow a section pane to all / set / modified options.
    SetOptionFilter(OptionFilter),
    /// Escape: back out of whatever is in front (modal → panel → search).
    Escape,
    /// Show/hide the keyboard-shortcut sheet.
    ToggleShortcuts,
    /// Clear the transient save / `hyprctl` status line.
    DismissStatus,
    /// Split a gap option into per-side fields, or link the sides back into
    /// one value.
    ToggleGapSides(String),
    /// Expand/collapse a collection entry's editor.
    ToggleRow(CollectionId, usize),
    /// The collection filter box changed.
    CollectionFilter(String),
}

/// Top-level application state.
#[derive(Debug)]
pub(crate) struct App {
    pub(crate) theme: Theme,
    pub(crate) schema: &'static Schema,
    pub(crate) load: LoadState,
    pub(crate) selected: Selection,
    pub(crate) search: String,
    pub(crate) show_changes: bool,
    /// Whether the save panel is open.
    pub(crate) show_save: bool,
    /// Whether the diagnostics panel (load-time issues) is open.
    pub(crate) show_diagnostics: bool,
    /// The chosen output format (defaults to the loaded format).
    pub(crate) output_format: Option<ConfigFormat>,
    /// "Save despite soft warnings".
    pub(crate) override_warnings: bool,
    /// The last save's status line, if any.
    pub(crate) save_status: Option<Result<String, String>>,
    /// Undo history (newest last).
    pub(crate) undo: VecDeque<EditSnapshot>,
    /// Redo history (newest last).
    pub(crate) redo: VecDeque<EditSnapshot>,
    /// The coalescing key of the last continuous edit, if any.
    pub(crate) last_key: Option<String>,
    /// A detected running Hyprland, if any.
    pub(crate) hyprland: Option<HyprlandInfo>,
    /// The displays currently attached, newest detection wins. Empty when
    /// Hyprland isn't running or `hyprctl` failed.
    pub(crate) outputs: Vec<DetectedMonitor>,
    /// Why the last output detection failed, if it did.
    pub(crate) outputs_error: Option<String>,
    /// Monitor cards whose advanced fields are expanded, by connector.
    pub(crate) expanded_monitors: HashSet<String>,
    /// Whether committed scalar edits are pushed live via `hyprctl`.
    pub(crate) live_apply: bool,
    /// The last `hyprctl` status line, if any.
    pub(crate) hypr_status: Option<Result<String, String>>,
    /// Persisted settings (theme/format/window/recents).
    pub(crate) settings: Settings,
    /// Whether the profiles & recents panel is open.
    pub(crate) show_profiles: bool,
    /// The in-progress profile name.
    pub(crate) profile_name: String,
    /// The in-progress import path.
    pub(crate) import_path: String,
    /// The open color picker, if any (live HSV state for one option).
    pub(crate) color_picker: Option<ColorDraft>,
    /// The save panel's precomputed plan/validation/diffs. Built in `update`
    /// (never in `view`) whenever the panel is open and its inputs change.
    pub(crate) save_preview: Option<save::SavePreview>,
    /// When the window size was last written to disk, to throttle the otherwise
    /// per-event writes during a continuous drag-resize. `None` until first set.
    pub(crate) last_window_persist: Option<Instant>,
    /// The in-progress `.conf` -> Lua migration, if the flow is open.
    pub(crate) migration: Option<Migration>,
    /// Whether the `.conf` deprecation banner was dismissed this session.
    pub(crate) deprecation_dismissed: bool,
    /// Set by `--migrate`: open the conversion flow once the config has loaded.
    pub(crate) migrate_on_load: bool,

    // -- derived caches -------------------------------------------------
    // Everything below is a *projection* of the state above. `view` runs on
    // every frame, so anything that costs more than a field read is computed
    // here in `update` instead — once per change rather than once per frame.
    /// Scored search hits for the current query. Rebuilt only when the query
    /// (or the schema-relevant state) changes.
    pub(crate) hits: fuzzy::SearchIndex,
    /// Saved profiles. Refreshed when the panel opens or a profile is written,
    /// so `view` never touches the filesystem.
    pub(crate) profiles: Vec<profiles::Profile>,
    /// How many set options need a newer Hyprland than the detected one.
    pub(crate) stale_options: usize,
    /// Unsaved-edit counts per schema section, indexed like `schema.sections()`.
    pub(crate) dirty_by_section: Vec<u32>,
    /// The live window width. `settings.window_width` is the *persisted* value
    /// and lags a resize; layout decisions must use this one.
    pub(crate) window_width: f32,
    /// Which options the section panes show.
    pub(crate) option_filter: OptionFilter,
    /// Whether the keyboard-shortcut sheet is open.
    pub(crate) show_shortcuts: bool,
    /// Gap options the user split into per-side fields (a uniform value would
    /// otherwise collapse back to a single field on the next frame).
    pub(crate) split_gaps: HashSet<String>,
    /// Collection entries whose editors are expanded.
    pub(crate) expanded_rows: HashSet<(CollectionId, usize)>,
    /// The collection pane's filter text.
    pub(crate) collection_filter: String,
}

impl App {
    /// Boot: build the initial state and kick off the (non-blocking) load and
    /// Hyprland detection.
    fn boot(
        explicit: Option<PathBuf>,
        settings: Settings,
        migrate_on_load: bool,
    ) -> (Self, Task<Message>) {
        let schema = Schema::shared();
        let selected = schema
            .sections()
            .first()
            .map(|s| Selection::Section(s.id.clone()))
            .unwrap_or(Selection::Collection(CollectionId::Keybinds));

        let window_width = settings.window_width;
        let app = Self {
            theme: theme_from_name(&settings.theme),
            schema,
            load: LoadState::Loading,
            selected,
            search: String::new(),
            show_changes: false,
            show_save: false,
            show_diagnostics: false,
            output_format: Some(format_from_name(&settings.last_format)),
            override_warnings: false,
            save_status: None,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            last_key: None,
            hyprland: None,
            outputs: Vec::new(),
            outputs_error: None,
            expanded_monitors: HashSet::new(),
            live_apply: false,
            hypr_status: None,
            settings,
            show_profiles: false,
            profile_name: String::new(),
            import_path: String::new(),
            color_picker: None,
            save_preview: None,
            last_window_persist: None,
            migration: None,
            deprecation_dismissed: false,
            migrate_on_load,

            hits: fuzzy::SearchIndex::default(),
            profiles: Vec::new(),
            stale_options: 0,
            dirty_by_section: vec![0; schema.sections().len()],
            window_width,
            option_filter: OptionFilter::default(),
            show_shortcuts: false,
            split_gaps: HashSet::new(),
            expanded_rows: HashSet::new(),
            collection_filter: String::new(),
        };

        let load = Task::perform(async move { load::load_config(explicit) }, |state| {
            Message::Loaded(Arc::new(state))
        });
        let detect = Task::perform(
            async { hyprconf_core::hyprctl::detect() },
            Message::HyprlandDetected,
        );

        (app, Task::batch([load, detect, detect_monitors()]))
    }

    fn title(&self) -> String {
        format!("hyprconf {}", env!("CARGO_PKG_VERSION"))
    }

    fn theme(&self) -> Theme {
        self.theme.clone()
    }

    fn subscription(&self) -> iced::Subscription<Message> {
        iced::Subscription::batch([
            iced::event::listen_with(handle_event),
            iced::window::resize_events()
                .map(|(_id, size)| Message::WindowResized(size.width, size.height)),
        ])
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ThemeSelected(theme) => {
                self.settings.theme = theme.to_string();
                self.settings.save();
                self.theme = theme;
            }
            Message::Loaded(state) => {
                match &*state {
                    LoadState::Loaded(loaded) => tracing::info!(
                        format = load::format_label(loaded.format),
                        source = %loaded.source.display(),
                        options = loaded.config.option_count(),
                        warnings = loaded.diagnostics.len(),
                        "configuration loaded",
                    ),
                    LoadState::NotFound { searched } => {
                        tracing::warn!(?searched, "no configuration found");
                    }
                    LoadState::Error { path, message } => {
                        tracing::error!(path = %path.display(), %message, "failed to load configuration");
                    }
                    LoadState::Loading => {}
                }
                // The message holds the only `Arc`, so unwrap it to take the
                // `LoadState` (incl. the parsed `ConfBundle`) without deep-cloning.
                self.load = Arc::try_unwrap(state).unwrap_or_else(|arc| (*arc).clone());
                // A fresh load invalidates the edit history, the open picker
                // and any per-row UI state.
                self.undo.clear();
                self.redo.clear();
                self.last_key = None;
                self.color_picker = None;
                self.expanded_rows.clear();
                self.split_gaps.clear();

                let recent = match &self.load {
                    LoadState::Loaded(loaded) => Some(loaded.source.display().to_string()),
                    _ => None,
                };
                if let Some(source) = recent {
                    self.settings.add_recent(&source);
                    self.settings.save();
                }
                self.refresh_save_preview();
                self.refresh_dirty_index();
                self.refresh_stale();

                // `--migrate` opens the conversion flow as soon as there is
                // something to convert. Consumed once so re-loading a file later
                // does not re-open it.
                if std::mem::take(&mut self.migrate_on_load) && self.show_deprecation_banner() {
                    return Task::done(Message::StartMigration);
                }
            }
            Message::Selected(selection) => {
                if self.selected != selection {
                    self.collection_filter.clear();
                }
                self.selected = selection;
                self.search.clear();
                self.hits = fuzzy::SearchIndex::default();
                self.show_changes = false;
                self.show_save = false;
                self.show_profiles = false;
                self.show_diagnostics = false;
                self.color_picker = None;
            }
            Message::SearchChanged(query) => {
                // Scoring the whole schema happens here, once per keystroke —
                // never in `view`, which runs on every frame.
                self.hits = fuzzy::SearchIndex::build(self.schema, &query);
                self.search = query;
            }
            Message::Edit(action) => return self.apply_edit(action),
            Message::CollectionEdit(action) => {
                self.record(action.coalesce_key());
                let id = action.collection();
                // Row state is index-based: anything that shifts indices
                // invalidates it. A freshly added entry opens ready to edit.
                let added = matches!(action, edit::CollectionAction::Add(_));
                if action.is_structural() {
                    self.expanded_rows.retain(|(c, _)| *c != id);
                }
                if let LoadState::Loaded(loaded) = &mut self.load {
                    loaded.apply_collection(action);
                }
                if added {
                    let count = self.collection_len(id);
                    if count > 0 {
                        self.expanded_rows.insert((id, count - 1));
                    }
                    self.collection_filter.clear();
                }
            }
            Message::ToggleRow(id, index) => {
                if !self.expanded_rows.remove(&(id, index)) {
                    self.expanded_rows.insert((id, index));
                }
            }
            Message::CollectionFilter(text) => self.collection_filter = text,
            Message::ToggleGapSides(path) => {
                let split = self.split_gaps.contains(&path)
                    || self.load.loaded().is_some_and(|l| {
                        self.schema
                            .option(&path)
                            .is_some_and(|o| !l.current_gap(&path, o).is_uniform())
                    });
                if split {
                    // Link: every side takes the top value.
                    self.split_gaps.remove(&path);
                    let top = self
                        .load
                        .loaded()
                        .zip(self.schema.option(&path))
                        .map_or(0, |(l, o)| l.current_gap(&path, o).top);
                    return self.apply_edit(EditAction::SetGap(
                        path,
                        hyprconf_core::value::CssGap::uniform(top),
                    ));
                }
                self.split_gaps.insert(path);
            }
            Message::Undo => {
                self.last_key = None;
                let Some(prev) = self.undo.pop_back() else {
                    return Task::none();
                };
                let lens = self.collection_lens();
                if let LoadState::Loaded(loaded) = &mut self.load {
                    let current = loaded.snapshot();
                    loaded.restore(prev);
                    self.redo.push_back(current);
                } else {
                    self.undo.push_back(prev);
                }
                self.prune_rows(&lens);
                self.refresh_save_preview();
                self.refresh_dirty_index();
            }
            Message::Redo => {
                self.last_key = None;
                let Some(next) = self.redo.pop_back() else {
                    return Task::none();
                };
                let lens = self.collection_lens();
                if let LoadState::Loaded(loaded) = &mut self.load {
                    let current = loaded.snapshot();
                    loaded.restore(next);
                    self.undo.push_back(current);
                } else {
                    self.redo.push_back(next);
                }
                self.prune_rows(&lens);
                self.refresh_save_preview();
                self.refresh_dirty_index();
            }
            Message::ToggleChanges => {
                self.show_changes = !self.show_changes;
                if self.show_changes {
                    self.show_save = false;
                    self.show_profiles = false;
                    self.show_diagnostics = false;
                }
            }
            Message::ToggleSave => {
                self.show_save = !self.show_save;
                if self.show_save {
                    self.show_changes = false;
                    self.show_profiles = false;
                    self.show_diagnostics = false;
                    self.save_status = None;
                    if self.output_format.is_none() {
                        self.output_format = self.load.loaded().map(|l| l.format);
                    }
                }
                self.refresh_save_preview();
            }
            Message::ToggleDiagnostics => {
                self.show_diagnostics = !self.show_diagnostics;
                if self.show_diagnostics {
                    self.show_changes = false;
                    self.show_save = false;
                    self.show_profiles = false;
                }
            }
            Message::SetOutputFormat(format) => {
                self.output_format = Some(format);
                self.save_status = None;
                self.settings.last_format = format_to_name(format).to_string();
                self.settings.save();
                self.refresh_save_preview();
            }
            Message::ToggleOverride(value) => self.override_warnings = value,
            Message::PerformSave => return self.perform_save(),
            Message::HyprlandDetected(info) => {
                if info.is_none() {
                    self.live_apply = false;
                }
                tracing::info!(detected = info.is_some(), "hyprland detection");
                self.hyprland = info;
                self.refresh_stale();
            }
            Message::MonitorsDetected(result) => match &*result {
                Ok(outputs) => {
                    tracing::info!(count = outputs.len(), "output detection");
                    self.outputs_error = None;
                    self.outputs = Arc::try_unwrap(result)
                        .unwrap_or_else(|arc| (*arc).clone())
                        .unwrap_or_default();
                }
                Err(message) => {
                    // Not an error the user needs shouting about: no Hyprland,
                    // no displays. The screen degrades to the raw rule list.
                    tracing::debug!(error = %message, "output detection unavailable");
                    self.outputs.clear();
                    self.outputs_error = Some(message.clone());
                }
            },
            Message::RefreshMonitors => return detect_monitors(),
            Message::MonitorEdit(connector, edit) => {
                return self.edit_monitor(&connector, edit);
            }
            Message::MonitorDropped(connector) => {
                // Applied once on release rather than on every drag frame, so a
                // drag costs one `hyprctl` call instead of hundreds.
                return self.live_apply_monitor(&connector);
            }
            Message::ToggleMonitorAdvanced(connector) => {
                if !self.expanded_monitors.remove(&connector) {
                    self.expanded_monitors.insert(connector);
                }
            }
            Message::ToggleLiveApply(on) => {
                self.live_apply = on && self.hyprland.is_some();
            }
            Message::Reload => {
                return Task::perform(
                    async { hyprconf_core::hyprctl::reload().map_err(|e| e.to_string()) },
                    Message::HyprResult,
                );
            }
            Message::HyprResult(result) => {
                match &result {
                    Ok(msg) => tracing::info!(message = %msg, "hyprctl ok"),
                    Err(e) => tracing::warn!(error = %e, "hyprctl failed"),
                }
                self.hypr_status = Some(result);
            }
            Message::WindowResized(width, height) => {
                // Layout reads `self.window_width`, which is always current;
                // `settings` only mirrors it for the *next* launch and is
                // written to disk at most a few times a second (below).
                self.window_width = width;
                self.settings.window_width = width;
                self.settings.window_height = height;
                // Throttle disk writes to ~2/sec so a continuous drag-resize
                // doesn't write settings.toml on every event. The first resize
                // persists immediately; the exact final size is also captured by
                // any later `settings.save()` (theme/format/recents).
                let due = self
                    .last_window_persist
                    .is_none_or(|t| t.elapsed() >= Duration::from_millis(400));
                if due {
                    self.settings.save();
                    self.last_window_persist = Some(Instant::now());
                }
            }
            Message::ToggleProfiles => {
                self.show_profiles = !self.show_profiles;
                if self.show_profiles {
                    self.show_changes = false;
                    self.show_save = false;
                    self.show_diagnostics = false;
                    self.save_status = None;
                    // Read the directory here, not in `view` — a `read_dir` per
                    // frame is filesystem I/O on the render path.
                    self.profiles = profiles::list();
                }
            }
            Message::ProfileNameChanged(name) => self.profile_name = name,
            Message::SaveProfile => {
                if let Some(loaded) = self.load.loaded() {
                    let format = self.output_format.unwrap_or(loaded.format);
                    self.save_status = Some(
                        profiles::save(&self.profile_name, format, &loaded.config)
                            .map(|path| format!("Saved profile → {}", path.display())),
                    );
                    self.profiles = profiles::list();
                }
            }
            Message::ImportPathChanged(path) => self.import_path = path,
            Message::OpenPath(path) => {
                return Task::perform(async move { load::load_config(Some(path)) }, |state| {
                    Message::Loaded(Arc::new(state))
                });
            }
            Message::OpenColorPicker(path) => {
                let target = ColorTarget::Option(path);
                let color = self.target_color(&target);
                self.color_picker = Some(ColorDraft::from_color(target, color));
            }
            Message::OpenStopColorPicker(path, index) => {
                let target = ColorTarget::Stop { path, index };
                let color = self.target_color(&target);
                self.color_picker = Some(ColorDraft::from_color(target, color));
            }
            Message::CloseColorPicker => self.color_picker = None,
            Message::PickSatVal(sat, val) => {
                let Some(cp) = self.color_picker.as_mut() else {
                    return Task::none();
                };
                cp.sat = sat;
                cp.val = val;
                let (target, hue) = (cp.target.clone(), cp.hue);
                let color = Color::from_hsv(
                    f64::from(hue),
                    f64::from(sat),
                    f64::from(val),
                    self.target_alpha(&target),
                );
                return self.apply_pick(pick_action(&target, color));
            }
            Message::PickHue(hue) => {
                let Some(cp) = self.color_picker.as_mut() else {
                    return Task::none();
                };
                cp.hue = hue;
                let (target, sat, val) = (cp.target.clone(), cp.sat, cp.val);
                let color = Color::from_hsv(
                    f64::from(hue),
                    f64::from(sat),
                    f64::from(val),
                    self.target_alpha(&target),
                );
                return self.apply_pick(pick_action(&target, color));
            }

            // -- migration ---------------------------------------------
            Message::StartMigration => {
                let Some(loaded) = self.load.loaded() else {
                    return Task::none();
                };
                tracing::info!(source = ?loaded.source, "starting .conf -> lua migration");
                self.migration = Some(Migration::start(loaded));
                self.close_panels();
                // Check eagerly: the verdict is the whole point of the flow, and
                // it is cheap enough that the user should never have to ask.
                return Task::done(Message::MigrateCheck);
            }
            Message::MigrateGoto(step) => {
                if let Some(m) = self.migration.as_mut() {
                    m.step = step;
                }
            }
            Message::MigrateCheck => {
                let Some(m) = self.migration.as_mut() else {
                    return Task::none();
                };
                m.checking = true;
                let lua = m.lua.clone();
                return Task::perform(
                    async move { hyprconf_core::verify::verify_text(&lua, "lua") },
                    |verdict| Message::MigrateChecked(Box::new(verdict)),
                );
            }
            Message::MigrateChecked(verdict) => {
                if let Some(m) = self.migration.as_mut() {
                    m.checking = false;
                    tracing::info!(ok = verdict.is_ok(), "migration check finished");
                    m.verdict = Some(*verdict);
                }
            }
            Message::MigrateOverride(on) => {
                if let Some(m) = self.migration.as_mut() {
                    m.override_check = on;
                }
            }
            Message::MigrateApply => {
                let Some(m) = self.migration.as_mut() else {
                    return Task::none();
                };
                m.step = MigrateStep::Done;
                let (target, lua) = (m.target.clone(), m.lua.clone());
                return Task::perform(async move { migrate::apply(target, lua) }, |result| {
                    Message::MigrateApplied(Box::new(result))
                });
            }
            Message::MigrateApplied(result) => {
                let result = *result;
                match &result {
                    Ok(a) => tracing::info!(written = ?a.written, "migration applied"),
                    Err(e) => tracing::error!(error = %e, "migration failed"),
                }
                let reload = result.as_ref().ok().map(|a| a.written.clone());
                if let Some(m) = self.migration.as_mut() {
                    m.outcome = Some(result);
                }
                // Re-open the freshly written Lua so the editor now works against
                // the file Hyprland will actually read.
                if let Some(path) = reload {
                    self.settings.last_format = "lua".to_string();
                    self.settings.save();
                    self.output_format = Some(ConfigFormat::Lua);
                    return Task::perform(async move { load::load_config(Some(path)) }, |state| {
                        Message::Loaded(Arc::new(state))
                    });
                }
            }
            Message::CloseMigration => self.migration = None,
            Message::DismissDeprecation => self.deprecation_dismissed = true,
            Message::FocusSearch => {
                return iced::advanced::widget::operate(
                    iced::advanced::widget::operation::focusable::focus(view::SEARCH_ID.into()),
                );
            }
            Message::SetOptionFilter(filter) => self.option_filter = filter,
            Message::ToggleShortcuts => self.show_shortcuts = !self.show_shortcuts,
            Message::DismissStatus => {
                self.save_status = None;
                self.hypr_status = None;
            }
            Message::Escape => return self.escape(),
        }
        Task::none()
    }

    /// Back out of exactly one layer, outermost first.
    ///
    /// Escape that closes *everything* is as annoying as Escape that closes
    /// nothing: the user loses context they didn't ask to lose. So this peels
    /// one layer per press, in the order things visually stack.
    fn escape(&mut self) -> Task<Message> {
        if self.show_shortcuts {
            self.show_shortcuts = false;
        } else if self.color_picker.is_some() {
            self.color_picker = None;
        } else if !self.search.is_empty() {
            self.search.clear();
            self.hits = fuzzy::SearchIndex::default();
        } else if self.show_save || self.show_changes || self.show_profiles || self.show_diagnostics
        {
            self.close_panels();
            self.save_preview = None;
        } else if self.save_status.is_some() || self.hypr_status.is_some() {
            self.save_status = None;
            self.hypr_status = None;
        }
        Task::none()
    }

    /// How many entries a collection holds right now.
    fn collection_len(&self, id: CollectionId) -> usize {
        let Some(loaded) = self.load.loaded() else {
            return 0;
        };
        let c = &loaded.config;
        match id {
            CollectionId::Monitors => c.monitors.len(),
            CollectionId::Workspaces => c.workspaces.len(),
            CollectionId::WindowRules => c.window_rules.len(),
            CollectionId::LayerRules => c.layer_rules.len(),
            CollectionId::Keybinds => c.keybinds.len(),
            CollectionId::Submaps => c.submaps.len(),
            CollectionId::Env => c.env.len(),
            CollectionId::Execs => c.execs.len(),
            CollectionId::Variables => c.variables.len(),
            CollectionId::Beziers => c.beziers.len(),
            CollectionId::Animations => c.animations.len(),
            CollectionId::Gestures => c.gestures.len(),
            CollectionId::Devices => c.devices.len(),
            CollectionId::Permissions => c.permissions.len(),
            CollectionId::Plugins => c.plugins.len(),
        }
    }

    /// Every collection's current length.
    fn collection_lens(&self) -> Vec<(CollectionId, usize)> {
        self.schema
            .collections()
            .iter()
            .map(|c| (c.id, self.collection_len(c.id)))
            .collect()
    }

    /// Forget expanded rows of collections whose length changed since `before`
    /// (an undone add/remove shifts every index after it). Field-level undos
    /// keep the row you are editing open.
    fn prune_rows(&mut self, before: &[(CollectionId, usize)]) {
        for &(id, len) in before {
            if self.collection_len(id) != len {
                self.expanded_rows.retain(|(c, _)| *c != id);
            }
        }
    }

    /// Recount unsaved scalar edits per section, for the sidebar badges.
    ///
    /// One pass over the schema (a few hundred set lookups) per *edit*, versus
    /// the same pass per *frame* if `view` did it.
    fn refresh_dirty_index(&mut self) {
        let Some(loaded) = self.load.loaded() else {
            self.dirty_by_section.iter_mut().for_each(|n| *n = 0);
            return;
        };
        self.dirty_by_section = self
            .schema
            .sections()
            .iter()
            .map(|section| {
                section
                    .options
                    .iter()
                    .filter(|o| loaded.is_dirty(&o.path))
                    .count() as u32
            })
            .collect();
    }

    /// Recount options the running Hyprland is too old for.
    ///
    /// Only the *count* is kept: the status bar shows a number, and recomputing
    /// the full problem list on every frame (which is what it used to do) is
    /// pure waste.
    fn refresh_stale(&mut self) {
        self.stale_options = match (self.load.loaded(), &self.hyprland) {
            (Some(loaded), Some(info)) => {
                hyprconf_core::unsupported_options(self.schema, &loaded.config, &info.version).len()
            }
            _ => 0,
        };
    }

    /// Close every side panel (used when a full-screen flow takes over).
    fn close_panels(&mut self) {
        self.show_changes = false;
        self.show_save = false;
        self.show_profiles = false;
        self.show_diagnostics = false;
        self.color_picker = None;
    }

    /// Whether the `.conf` deprecation banner should be shown right now.
    pub(crate) fn show_deprecation_banner(&self) -> bool {
        !self.deprecation_dismissed
            && self.migration.is_none()
            && self
                .load
                .loaded()
                .is_some_and(|l| migrate::should_warn(l.format))
    }

    /// Push an undo snapshot for an edit, coalescing consecutive continuous
    /// edits (typing/dragging) that share a `key` into a single step.
    fn record(&mut self, key: Option<String>) {
        let coalesce = key.is_some() && self.last_key == key;
        self.last_key = key;
        if coalesce {
            return;
        }
        let snapshot = self.load.loaded().map(Loaded::snapshot);
        if let Some(snapshot) = snapshot {
            self.undo.push_back(snapshot);
            self.redo.clear();
            const MAX_UNDO: usize = 200;
            if self.undo.len() > MAX_UNDO {
                self.undo.pop_front();
            }
        }
    }

    /// Apply an edit to the rule governing `connector`.
    ///
    /// The Monitors screen speaks in connectors; the file speaks in rules. This
    /// resolves one to the other, creating a rule seeded from the display's
    /// current state when the config has nothing to say about it — so merely
    /// touching a display never changes how it runs.
    fn edit_monitor(&mut self, connector: &str, edit: MonitorEdit) -> Task<Message> {
        // Position edits stream in during a drag and coalesce into one undo
        // step; the toggle deliberately does not (see `monitor_field_tag`).
        let key = (!matches!(edit, MonitorEdit::Enabled(..)))
            .then(|| format!("mon:{connector}:{}", edit::monitor_field_tag(&edit)));
        let dragging = matches!(edit, MonitorEdit::Position(_));
        self.record(key);

        let seed = self.monitor_seed(connector);
        if let LoadState::Loaded(loaded) = &mut self.load {
            loaded.edit_monitor(&seed, edit);
        }
        // A drag is applied on release instead — one `hyprctl` call, not one
        // per frame.
        if dragging {
            return Task::none();
        }
        self.live_apply_monitor(connector)
    }

    /// A rule reproducing a display's current state, used to seed a new entry.
    ///
    /// Falls back to Hyprland's own defaults when the display isn't detected
    /// (no compositor running), which is the best we can honestly do.
    fn monitor_seed(&self, connector: &str) -> MonitorRule {
        match self.outputs.iter().find(|o| o.name == connector) {
            Some(o) => MonitorRule {
                name: o.name.clone(),
                mode: o.current_mode(),
                position: o.current_position(),
                scale: crate::edit::fmt_num(o.scale),
                extra: Vec::new(),
            },
            None => MonitorRule {
                name: connector.to_string(),
                mode: "preferred".into(),
                position: "auto".into(),
                scale: "1".into(),
                extra: Vec::new(),
            },
        }
    }

    /// Push a monitor rule to the running Hyprland (`hyprctl keyword monitor`)
    /// and re-read the resulting layout, when live-apply is on.
    fn live_apply_monitor(&self, connector: &str) -> Task<Message> {
        if !self.live_apply || self.hyprland.is_none() {
            return Task::none();
        }
        let Some(loaded) = self.load.loaded() else {
            return Task::none();
        };
        let Some(i) = edit::monitor_rule_index(&loaded.config.monitors, connector) else {
            return Task::none();
        };
        let rule = loaded.config.monitors[i].value.clone();
        Task::perform(
            async move { hyprconf_core::hyprctl::apply_monitor(&rule).map_err(|e| e.to_string()) },
            Message::HyprResult,
        )
        // The compositor may not honour the request verbatim (an unsupported
        // mode, a layout it re-flows); re-reading keeps the view truthful.
        .chain(detect_monitors())
    }

    /// If live-apply is on and the edited option is valid, push it to the
    /// running Hyprland via `hyprctl keyword`.
    fn live_apply_task(&self, path: Option<String>) -> Task<Message> {
        if !self.live_apply || self.hyprland.is_none() {
            return Task::none();
        }
        let Some(path) = path else {
            return Task::none();
        };
        let Some(loaded) = self.load.loaded() else {
            return Task::none();
        };
        if loaded.first_error(&path).is_some() {
            return Task::none();
        }
        let Some(value) = loaded.config.get(&path) else {
            return Task::none();
        };
        // `apply_option` picks `hyprctl keyword` or `hyprctl eval` depending
        // on whether the session runs a `.conf` or a Lua config.
        let value = value.clone();
        Task::perform(
            async move {
                hyprconf_core::hyprctl::apply_option(&path, &value).map_err(|e| e.to_string())
            },
            Message::HyprResult,
        )
    }

    /// Apply a *picker* edit (from the 2D area / hue strip). The open picker's
    /// HSV is already authoritative, so — unlike [`App::apply_edit`] — we must
    /// NOT re-derive it from the model (that would lose hue/saturation at
    /// value→0 / saturation→0).
    fn apply_pick(&mut self, action: EditAction) -> Task<Message> {
        self.commit_edit(action, false)
    }

    /// Apply any other edit, then keep an open color picker's HSV in sync (so
    /// the area/strip track explicit hex & slider edits).
    fn apply_edit(&mut self, action: EditAction) -> Task<Message> {
        self.commit_edit(action, true)
    }

    /// Shared edit path: snapshot for undo, mutate the model, optionally re-sync
    /// an open color picker, then live-apply the committed scalar.
    fn commit_edit(&mut self, action: EditAction, sync_picker: bool) -> Task<Message> {
        self.record(action.coalesce_key());
        let path = action.option_path().map(str::to_string);
        if let LoadState::Loaded(loaded) = &mut self.load {
            loaded.apply(action, self.schema);
        }
        if sync_picker {
            if let Some(path) = &path {
                self.sync_color_picker(path);
            }
        }
        self.refresh_dirty_index();
        self.live_apply_task(path)
    }

    /// Re-derive the open picker's HSV from the model after a non-area edit to
    /// the same target (so the 2D area / hue strip track channel & hex edits).
    fn sync_color_picker(&mut self, path: &str) {
        let Some(target) = self.color_picker.as_ref().map(|cp| cp.target.clone()) else {
            return;
        };
        if target.path() != path {
            return;
        }
        let (h, s, v) = self.target_color(&target).to_hsv();
        if let Some(cp) = self.color_picker.as_mut() {
            cp.hue = h as f32;
            cp.sat = s as f32;
            cp.val = v as f32;
        }
    }

    /// The model's current color for a picker target (set value, else schema
    /// default, else opaque black / white).
    fn target_color(&self, target: &ColorTarget) -> Color {
        match target {
            ColorTarget::Option(path) => self.current_color(path),
            ColorTarget::Stop { path, index } => self.stop_color(path, *index),
        }
    }

    /// The current alpha for a picker target (defaults to fully opaque).
    fn target_alpha(&self, target: &ColorTarget) -> u8 {
        self.target_color(target).a
    }

    /// The model's current color for a scalar color option.
    fn current_color(&self, path: &str) -> Color {
        self.load
            .loaded()
            .and_then(|l| l.config.get(path))
            .and_then(value_color)
            .or_else(|| {
                self.schema
                    .option(path)
                    .map(|o| &o.default)
                    .and_then(value_color)
            })
            .unwrap_or(Color::rgba(0, 0, 0, 255))
    }

    /// The model's current color for one stop of a gradient option.
    fn stop_color(&self, path: &str, index: usize) -> Color {
        let from = |value: &Value| match value {
            Value::Gradient(g) => g.stops.get(index).copied(),
            _ => None,
        };
        self.load
            .loaded()
            .and_then(|l| l.config.get(path))
            .and_then(from)
            .or_else(|| self.schema.option(path).map(|o| &o.default).and_then(from))
            .unwrap_or(Color::rgba(255, 255, 255, 255))
    }

    /// Rebuild (or clear) the cached [`save::SavePreview`] so it matches the
    /// current model and output format. Cheap when the save panel is closed; the
    /// (expensive) plan/validation/diff work happens only while it is open, and
    /// only here in `update` — never in `view`.
    fn refresh_save_preview(&mut self) {
        if !self.show_save {
            self.save_preview = None;
            return;
        }
        let preview = self.load.loaded().map(|loaded| {
            let target = self.output_format.unwrap_or(loaded.format);
            save::build_preview(loaded, target, self.schema)
        });
        self.save_preview = preview;
    }

    /// Validate, write the plan, and reload from disk on success.
    fn perform_save(&mut self) -> Task<Message> {
        let outcome: Option<(Result<String, String>, Option<PathBuf>)> = match self.load.loaded() {
            Some(loaded) => {
                let target = self.output_format.unwrap_or(loaded.format);
                let plan = save::plan_save(loaded, target);
                let mut problems = save::review(loaded, self.schema);
                problems.extend(save::plan_problems(&plan));
                if let Some(reason) = save::blocked(&problems, self.override_warnings) {
                    Some((Err(reason), None))
                } else {
                    match save::perform_save(&plan) {
                        Ok(reports) => {
                            let backups = reports.iter().filter(|r| r.backup.is_some()).count();
                            let summary = format!(
                                "Saved {} file(s){}",
                                reports.len(),
                                if backups > 0 {
                                    format!(" · {backups} backup(s)")
                                } else {
                                    String::new()
                                }
                            );
                            Some((Ok(summary), Some(plan.root)))
                        }
                        Err(e) => Some((Err(format!("write failed: {e}")), None)),
                    }
                }
            }
            None => None,
        };

        if let Some((status, reload)) = outcome {
            self.save_status = Some(status);
            if let Some(root) = reload {
                self.show_save = false;
                self.override_warnings = false;
                self.output_format = None;
                return Task::perform(async move { load::load_config(Some(root)) }, |state| {
                    Message::Loaded(Arc::new(state))
                });
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        view::view(self)
    }
}

/// Re-read the attached displays off the UI thread.
fn detect_monitors() -> Task<Message> {
    Task::perform(
        async { hyprconf_core::hyprctl::monitors().map_err(|e| e.to_string()) },
        |result| Message::MonitorsDetected(Arc::new(result)),
    )
}

/// Translate a raw window event into a [`Message`] (keyboard shortcuts).
///
/// Escape is handled without a modifier — it is the one key users reach for to
/// get out of something, and requiring Ctrl for it would defeat the point.
fn handle_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    use iced::keyboard::key::Named;
    use iced::keyboard::{Event as KeyEvent, Key};

    let iced::Event::Keyboard(KeyEvent::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    if !modifiers.command() {
        return match key.as_ref() {
            Key::Named(Named::Escape) => Some(Message::Escape),
            _ => None,
        };
    }
    match key.as_ref() {
        Key::Character("z") if modifiers.shift() => Some(Message::Redo),
        Key::Character("z") => Some(Message::Undo),
        Key::Character("y") => Some(Message::Redo),
        Key::Character("s") => Some(Message::ToggleSave),
        // Ctrl+K alongside Ctrl+F: the command-palette muscle memory most
        // people arrive with.
        Key::Character("f" | "k") => Some(Message::FocusSearch),
        Key::Character("p") => Some(Message::ToggleProfiles),
        Key::Character("/") => Some(Message::ToggleShortcuts),
        _ => None,
    }
}

/// Extract a [`Color`] from a [`Value`], if it is one.
fn value_color(value: &Value) -> Option<Color> {
    match value {
        Value::Color(c) => Some(*c),
        _ => None,
    }
}

/// The edit that applies `color` to a picker target.
fn pick_action(target: &ColorTarget, color: Color) -> EditAction {
    match target {
        ColorTarget::Option(path) => EditAction::SetColor(path.clone(), color),
        ColorTarget::Stop { path, index } => EditAction::SetStopColor(path.clone(), *index, color),
    }
}

/// Resolve a persisted theme name to a [`Theme`] (defaults to Catppuccin Mocha).
fn theme_from_name(name: &str) -> Theme {
    Theme::ALL
        .iter()
        .find(|t| t.to_string() == name)
        .cloned()
        .unwrap_or(Theme::CatppuccinMocha)
}

/// Resolve a persisted format name to a [`ConfigFormat`].
fn format_from_name(name: &str) -> ConfigFormat {
    if name.eq_ignore_ascii_case("lua") {
        ConfigFormat::Lua
    } else {
        ConfigFormat::Conf
    }
}

/// The persisted name for a [`ConfigFormat`].
fn format_to_name(format: ConfigFormat) -> &'static str {
    match format {
        ConfigFormat::Lua => "lua",
        ConfigFormat::Conf => "conf",
    }
}

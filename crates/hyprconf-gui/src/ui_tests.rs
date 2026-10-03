// SPDX-License-Identifier: MIT OR Apache-2.0
//! Headless UI tests: drive the real `App` (boot → `update` → `view`) through
//! the key flows without opening a window or a renderer. `update` exercises all
//! application logic; calling `view` builds the full Iced element tree (which is
//! renderer-independent), so these catch panics and state regressions across the
//! load → edit → add keybind → choose format → preview → save cycle.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};

use hyprconf_core::schema::CollectionId;
use hyprconf_core::{ConfigFormat, HyprlandInfo, Value};

use crate::color_picker::ColorTarget;
use crate::edit::{CollectionAction, EditAction, KeybindEdit};
use crate::load::{self, LoadState};
use crate::settings::Settings;
use crate::{view, App, Message, Selection};

/// Point XDG dirs at a throwaway location so `Settings`/profile writes during
/// `update` never touch the developer's real config. Set once per test binary.
fn init_xdg() {
    static XDG: Once = Once::new();
    XDG.call_once(|| {
        let base = std::env::temp_dir().join(format!("hyprconf-uitest-xdg-{}", std::process::id()));
        std::env::set_var("XDG_CONFIG_HOME", base.join("config"));
        std::env::set_var("XDG_DATA_HOME", base.join("data"));
    });
}

/// A unique temp file path (its parent directory is created).
fn temp_path(tag: &str, name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("hyprconf-ui-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// Boot the app and synchronously complete the initial load (boot's load runs in
/// a `Task` we don't have a runtime for, so we feed the result directly).
fn boot_loaded(path: &Path) -> App {
    init_xdg();
    let (mut app, _task) = App::boot(Some(path.to_path_buf()), Settings::default(), false);
    let state = load::load_config(Some(path.to_path_buf()));
    let _ = app.update(Message::Loaded(Arc::new(state)));
    app
}

#[test]
fn full_flow_load_edit_addbind_convert_preview_save() {
    // 1. Load a real (temp) .conf.
    let path = temp_path("flow", "hyprland.conf");
    std::fs::write(
        &path,
        "decoration:rounding = 4\nbind = SUPER, Q, killactive\n",
    )
    .unwrap();
    let mut app = boot_loaded(&path);
    assert!(matches!(app.load, LoadState::Loaded(_)));
    assert_eq!(
        app.load.loaded().unwrap().config.get("decoration:rounding"),
        Some(&Value::Int(4))
    );

    // 2. Edit a scalar.
    let _ = app.update(Message::Edit(EditAction::SetIntSlider(
        "decoration:rounding".into(),
        12,
    )));
    let loaded = app.load.loaded().unwrap();
    assert_eq!(
        loaded.config.get("decoration:rounding"),
        Some(&Value::Int(12))
    );
    assert!(loaded.is_dirty("decoration:rounding"));

    // 3. Add a keybind and give it a key.
    let before = app.load.loaded().unwrap().config.keybinds.len();
    let _ = app.update(Message::CollectionEdit(CollectionAction::Add(
        CollectionId::Keybinds,
    )));
    let _ = app.update(Message::CollectionEdit(CollectionAction::Keybind(
        before,
        KeybindEdit::Key("T".into()),
    )));
    assert_eq!(app.load.loaded().unwrap().config.keybinds.len(), before + 1);

    // 4. Choose Lua output and open the save panel (preview).
    let _ = app.update(Message::SetOutputFormat(ConfigFormat::Lua));
    let _ = app.update(Message::ToggleSave);
    assert!(app.show_save);
    // Building the view must not panic (renders the conversion diff/preview).
    let _ = crate::view::view(&app);

    // 5. Save — converts to a sibling hyprland.lua.
    let _ = app.update(Message::PerformSave);
    assert!(
        matches!(app.save_status, Some(Ok(_))),
        "save status: {:?}",
        app.save_status
    );
    let lua_path = path.with_extension("lua");
    let written = std::fs::read_to_string(&lua_path).expect("hyprland.lua written");
    assert!(written.contains("hl.bind"), "{written}");
    assert!(written.contains("rounding = 12"), "{written}");

    // 6. Reload the written Lua and confirm the full cycle preserved everything.
    let reloaded = load::load_config(Some(lua_path.clone()));
    let _ = app.update(Message::Loaded(Arc::new(reloaded)));
    let l = app.load.loaded().unwrap();
    assert_eq!(l.format, ConfigFormat::Lua);
    assert_eq!(l.config.get("decoration:rounding"), Some(&Value::Int(12)));
    assert!(l.config.keybinds.iter().any(|t| t.value.key == "T"));

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn undo_redo_round_trips_a_scalar_edit() {
    let path = temp_path("undo", "hyprland.conf");
    std::fs::write(&path, "decoration:rounding = 4\n").unwrap();
    let mut app = boot_loaded(&path);

    let _ = app.update(Message::Edit(EditAction::SetIntSlider(
        "decoration:rounding".into(),
        20,
    )));
    assert_eq!(
        app.load.loaded().unwrap().config.get("decoration:rounding"),
        Some(&Value::Int(20))
    );

    let _ = app.update(Message::Undo);
    assert_eq!(
        app.load.loaded().unwrap().config.get("decoration:rounding"),
        Some(&Value::Int(4)),
        "undo restores the loaded value"
    );

    let _ = app.update(Message::Redo);
    assert_eq!(
        app.load.loaded().unwrap().config.get("decoration:rounding"),
        Some(&Value::Int(20)),
        "redo re-applies the edit"
    );

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn views_build_headlessly_across_panels() {
    let path = temp_path("views", "hyprland.conf");
    std::fs::write(
        &path,
        "decoration:rounding = 4\ngeneral:col.active_border = rgba(33ccffee) rgba(00ff99ee) 45deg\ngeneral:not_a_real_key = 1\n",
    )
    .unwrap();
    let mut app = boot_loaded(&path);
    // The unknown key above is preserved but surfaced as a diagnostic.
    assert!(
        !app.load.loaded().unwrap().diagnostics.is_empty(),
        "unknown option should produce a diagnostic"
    );

    // Search.
    let _ = app.update(Message::SearchChanged("round".into()));
    let _ = crate::view::view(&app);
    let _ = app.update(Message::SearchChanged(String::new()));

    // Navigate to a collection and back to a section.
    let _ = app.update(Message::Selected(Selection::Collection(
        CollectionId::Keybinds,
    )));
    let _ = crate::view::view(&app);
    let _ = app.update(Message::Selected(Selection::Section("decoration".into())));
    let _ = crate::view::view(&app);

    // Pending-changes and profiles panels.
    let _ = app.update(Message::ToggleChanges);
    let _ = crate::view::view(&app);
    let _ = app.update(Message::ToggleProfiles);
    let _ = crate::view::view(&app);

    // Diagnostics panel: opening it closes the others and renders the issue list.
    let _ = app.update(Message::ToggleDiagnostics);
    assert!(app.show_diagnostics);
    assert!(!app.show_profiles);
    let _ = crate::view::view(&app);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn color_picker_open_pick_and_close() {
    let path = temp_path("color", "hyprland.conf");
    std::fs::write(&path, "misc:background_color = rgba(11223344)\n").unwrap();
    let mut app = boot_loaded(&path);

    // Open the picker for a scalar color option; the modal must build.
    let _ = app.update(Message::OpenColorPicker("misc:background_color".into()));
    assert!(matches!(
        app.color_picker.as_ref().map(|c| &c.target),
        Some(ColorTarget::Option(_))
    ));
    let _ = crate::view::view(&app);

    // Drag the saturation/value area and the hue strip — the model updates live.
    let _ = app.update(Message::PickSatVal(0.5, 0.5));
    let _ = app.update(Message::PickHue(200.0));
    assert!(matches!(
        app.load
            .loaded()
            .unwrap()
            .config
            .get("misc:background_color"),
        Some(Value::Color(_))
    ));

    // Open a gradient stop picker too.
    let _ = app.update(Message::OpenStopColorPicker(
        "general:col.active_border".into(),
        0,
    ));
    let _ = crate::view::view(&app);

    let _ = app.update(Message::CloseColorPicker);
    assert!(app.color_picker.is_none());

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

// ---------------------------------------------------------------------------
// search, filtering and escape
// ---------------------------------------------------------------------------

#[test]
fn search_results_are_cached_not_recomputed_per_frame() {
    let path = temp_path("search", "hyprland.conf");
    std::fs::write(&path, "decoration:rounding = 4\n").unwrap();
    let mut app = boot_loaded(&path);

    // Nothing typed: no work has been done and nothing is held.
    assert!(app.hits.is_empty());

    let _ = app.update(Message::SearchChanged("rounding".into()));
    assert!(!app.hits.is_empty(), "typing must populate the cache");
    assert_eq!(app.hits.options[0].spec.path, "decoration:rounding");
    // Hits borrow the `'static` schema, so the cap bounds the widgets built,
    // not the memory held.
    assert!(app.hits.options.len() <= crate::fuzzy::MAX_HITS);
    let _ = view::view(&app);

    // Navigating away drops the results instead of leaving them stale.
    let _ = app.update(Message::Selected(Selection::Section("general".into())));
    assert!(app.search.is_empty());
    assert!(app.hits.is_empty());
}

#[test]
fn escape_backs_out_one_layer_at_a_time() {
    let path = temp_path("escape", "hyprland.conf");
    std::fs::write(&path, "decoration:rounding = 4\n").unwrap();
    let mut app = boot_loaded(&path);

    // Stack up: a search, a panel, a modal.
    let _ = app.update(Message::SearchChanged("round".into()));
    let _ = app.update(Message::ToggleSave);
    let _ = app.update(Message::OpenColorPicker("misc:background_color".into()));

    let _ = app.update(Message::Escape);
    assert!(app.color_picker.is_none(), "modal closes first");
    assert!(!app.search.is_empty(), "…and nothing else moves");

    let _ = app.update(Message::Escape);
    assert!(app.search.is_empty(), "then the search clears");
    assert!(app.hits.is_empty());
    assert!(app.show_save, "…and the panel is still open");

    let _ = app.update(Message::Escape);
    assert!(!app.show_save, "then the panel closes");

    // Escape on a quiet screen is a no-op, not a surprise.
    let _ = app.update(Message::Escape);
    let _ = view::view(&app);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn section_filter_narrows_to_set_and_modified_options() {
    let path = temp_path("filter", "hyprland.conf");
    std::fs::write(&path, "decoration:rounding = 4\n").unwrap();
    let mut app = boot_loaded(&path);
    let _ = app.update(Message::Selected(Selection::Section("decoration".into())));

    let section = app.schema.section("decoration").unwrap();
    let total = section.options.len();
    let set = |app: &App| {
        section
            .options
            .iter()
            .filter(|o| app.load.loaded().unwrap().config.get(&o.path).is_some())
            .count()
    };
    assert!(set(&app) < total, "the fixture only sets one option");

    // Every filter must render without panicking, whatever it leaves visible.
    for filter in crate::OptionFilter::ALL {
        let _ = app.update(Message::SetOptionFilter(filter));
        assert_eq!(app.option_filter, filter);
        let _ = view::view(&app);
    }

    // "modified" is empty until something is actually edited…
    let loaded = app.load.loaded().unwrap();
    assert!(section
        .options
        .iter()
        .all(|o| !crate::OptionFilter::Modified.keeps(loaded, &o.path)));

    let _ = app.update(Message::Edit(EditAction::SetIntSlider(
        "decoration:rounding".into(),
        9,
    )));
    let loaded = app.load.loaded().unwrap();
    assert_eq!(
        section
            .options
            .iter()
            .filter(|o| crate::OptionFilter::Modified.keeps(loaded, &o.path))
            .count(),
        1,
        "…and holds exactly the edited option afterwards"
    );

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn sidebar_dirty_counts_track_edits_and_undo() {
    let path = temp_path("badges", "hyprland.conf");
    std::fs::write(&path, "decoration:rounding = 4\n").unwrap();
    let mut app = boot_loaded(&path);

    let index = app
        .schema
        .sections()
        .iter()
        .position(|s| s.id == "decoration")
        .unwrap();
    assert_eq!(app.dirty_by_section.len(), app.schema.sections().len());
    assert_eq!(app.dirty_by_section[index], 0);

    let _ = app.update(Message::Edit(EditAction::SetIntSlider(
        "decoration:rounding".into(),
        9,
    )));
    assert_eq!(app.dirty_by_section[index], 1);

    // Undoing back to the loaded value clears the badge — a section that is
    // back to how it was on disk must not keep claiming an edit.
    let _ = app.update(Message::Undo);
    assert_eq!(app.dirty_by_section[index], 0);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn stale_option_count_is_derived_not_recomputed_in_view() {
    let path = temp_path("stale", "hyprland.conf");
    std::fs::write(&path, "decoration:rounding_power = 3\n").unwrap();
    let mut app = boot_loaded(&path);

    // No compositor detected: nothing can be judged stale.
    assert_eq!(app.stale_options, 0);

    // An ancient Hyprland makes the `since`-tagged option unsupported…
    let _ = app.update(Message::HyprlandDetected(Some(HyprlandInfo {
        version: "0.41.0".into(),
        tag: None,
    })));
    assert_eq!(app.stale_options, 1);
    let _ = view::view(&app);

    // …and a current one clears it, without `view` ever doing the scan.
    let _ = app.update(Message::HyprlandDetected(Some(HyprlandInfo {
        version: "0.55.2".into(),
        tag: None,
    })));
    assert_eq!(app.stale_options, 0);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn profiles_are_listed_when_the_panel_opens_not_while_rendering() {
    let path = temp_path("profile-cache", "hyprland.conf");
    std::fs::write(&path, "decoration:rounding = 4\n").unwrap();
    let mut app = boot_loaded(&path);

    // Booting does not touch the profiles directory at all.
    assert!(app.profiles.is_empty());

    let _ = app.update(Message::ProfileNameChanged("laptop".into()));
    let _ = app.update(Message::SaveProfile);
    assert!(
        matches!(app.save_status, Some(Ok(_))),
        "save status: {:?}",
        app.save_status
    );
    assert!(
        app.profiles.iter().any(|p| p.name == "laptop"),
        "a written profile must appear without a re-render"
    );

    let _ = app.update(Message::ToggleProfiles);
    let _ = view::view(&app);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn shortcut_sheet_opens_and_closes() {
    let path = temp_path("shortcuts", "hyprland.conf");
    std::fs::write(&path, "decoration:rounding = 4\n").unwrap();
    let mut app = boot_loaded(&path);

    let _ = app.update(Message::ToggleShortcuts);
    assert!(app.show_shortcuts);
    let _ = view::view(&app);

    // Escape takes precedence over everything else while it is open.
    let _ = app.update(Message::Escape);
    assert!(!app.show_shortcuts);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn compact_layout_follows_the_live_window_size() {
    let path = temp_path("compact", "hyprland.conf");
    std::fs::write(&path, "decoration:rounding = 4\n").unwrap();
    let mut app = boot_loaded(&path);

    let _ = app.update(Message::WindowResized(700.0, 600.0));
    assert_eq!(app.window_width, 700.0);
    let _ = view::view(&app);

    let _ = app.update(Message::WindowResized(1400.0, 900.0));
    assert_eq!(app.window_width, 1400.0);
    let _ = view::view(&app);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn live_apply_gated_on_detection() {
    let path = temp_path("live", "hyprland.conf");
    std::fs::write(&path, "decoration:rounding = 4\n").unwrap();
    let mut app = boot_loaded(&path);

    // Without a detected Hyprland, live-apply cannot be turned on.
    let _ = app.update(Message::ToggleLiveApply(true));
    assert!(!app.live_apply);

    // With one detected, it can — and a subsequent edit builds its (unpolled)
    // hyprctl task without panicking.
    let _ = app.update(Message::HyprlandDetected(Some(HyprlandInfo {
        version: "0.55.2".into(),
        tag: None,
    })));
    let _ = app.update(Message::ToggleLiveApply(true));
    assert!(app.live_apply);
    let _ = app.update(Message::Edit(EditAction::SetIntSlider(
        "decoration:rounding".into(),
        6,
    )));
    assert_eq!(
        app.load.loaded().unwrap().config.get("decoration:rounding"),
        Some(&Value::Int(6))
    );

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

// ---------------------------------------------------------------------------
// monitors
// ---------------------------------------------------------------------------

/// Two attached displays, as `hyprctl monitors all -j` would report them.
fn fake_outputs() -> Vec<hyprconf_core::DetectedMonitor> {
    hyprconf_core::outputs::parse_monitors(
        r#"[{"name":"DP-1","description":"Dell Inc. AW3423DWF 0xABCD",
             "make":"Dell Inc.","model":"AW3423DWF",
             "width":3440,"height":1440,"refreshRate":164.9,
             "x":0,"y":0,"scale":1.0,"transform":0,"focused":true,"vrr":true,
             "currentFormat":"XRGB2101010","mirrorOf":"none",
             "availableModes":["3440x1440@164.90Hz","3440x1440@60.00Hz"]},
            {"name":"DP-3","description":"Samsung LF24T35 0x1111",
             "make":"Samsung","model":"LF24T35",
             "width":1920,"height":1080,"refreshRate":74.97,
             "x":3440,"y":0,"scale":1.0,"transform":0,
             "currentFormat":"XRGB8888","mirrorOf":"none",
             "availableModes":["1920x1080@74.97Hz"]}]"#,
    )
    .unwrap()
}

fn detected(app: &mut App) {
    let _ = app.update(Message::MonitorsDetected(Arc::new(Ok(fake_outputs()))));
    let _ = app.update(Message::Selected(Selection::Collection(
        CollectionId::Monitors,
    )));
}

#[test]
fn monitors_page_renders_with_and_without_detection() {
    let path = temp_path("monitors-empty", "hyprland.conf");
    std::fs::write(&path, "decoration:rounding = 4\n").unwrap();
    let mut app = boot_loaded(&path);

    // No compositor: the page must still build and say why it is empty.
    let _ = app.update(Message::Selected(Selection::Collection(
        CollectionId::Monitors,
    )));
    assert!(app.outputs.is_empty());
    let _ = view::view(&app);

    // A failed detection is recorded, not fatal.
    let _ = app.update(Message::MonitorsDetected(Arc::new(Err(
        "hyprctl is not available".into(),
    ))));
    assert_eq!(
        app.outputs_error.as_deref(),
        Some("hyprctl is not available")
    );
    let _ = view::view(&app);

    // With displays attached the cards and layout canvas render.
    detected(&mut app);
    assert_eq!(app.outputs.len(), 2);
    let _ = view::view(&app);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn editing_a_detected_display_authors_a_rule_seeded_from_its_current_state() {
    use crate::edit::{extra_field, MonitorEdit};

    let path = temp_path("monitors-edit", "hyprland.conf");
    std::fs::write(&path, "decoration:rounding = 4\n").unwrap();
    let mut app = boot_loaded(&path);
    detected(&mut app);
    assert!(app.load.loaded().unwrap().config.monitors.is_empty());

    // Rotating a display the config never mentioned creates one rule that
    // otherwise reproduces exactly how it is running.
    let _ = app.update(Message::MonitorEdit(
        "DP-3".into(),
        MonitorEdit::Transform("1".into()),
    ));
    let monitors = &app.load.loaded().unwrap().config.monitors;
    assert_eq!(monitors.len(), 1);
    let m = &monitors[0].value;
    assert_eq!(m.name, "DP-3");
    assert_eq!(m.mode, "1920x1080@74.97");
    assert_eq!(m.position, "3440x0");
    assert_eq!(extra_field(&m.extra, "transform"), "1");

    // Further edits reuse that rule instead of stacking duplicates.
    let _ = app.update(Message::MonitorEdit(
        "DP-3".into(),
        MonitorEdit::Scale("1.5".into()),
    ));
    assert_eq!(app.load.loaded().unwrap().config.monitors.len(), 1);
    assert_eq!(
        app.load.loaded().unwrap().config.monitors[0].value.scale,
        "1.5"
    );
    let _ = view::view(&app);

    // …and undo peels them back one at a time.
    let _ = app.update(Message::Undo);
    assert_eq!(
        app.load.loaded().unwrap().config.monitors[0].value.scale,
        "1"
    );
    let _ = app.update(Message::Undo);
    assert!(app.load.loaded().unwrap().config.monitors.is_empty());

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn dragging_a_display_writes_its_position_and_survives_a_drop() {
    use crate::edit::MonitorEdit;

    let path = temp_path("monitors-drag", "hyprland.conf");
    std::fs::write(&path, "monitor = DP-3, 1920x1080@74.97, 3440x0, 1\n").unwrap();
    let mut app = boot_loaded(&path);
    detected(&mut app);

    // The canvas streams positions while dragging…
    for x in [3400, 3200, 3000] {
        let _ = app.update(Message::MonitorEdit(
            "DP-3".into(),
            MonitorEdit::Position(format!("{x}x0")),
        ));
    }
    assert_eq!(
        app.load.loaded().unwrap().config.monitors[0].value.position,
        "3000x0"
    );
    // …and the drop (which would live-apply) is harmless without a compositor.
    let _ = app.update(Message::MonitorDropped("DP-3".into()));

    // The whole drag is a single undo step, not one per frame.
    let _ = app.update(Message::Undo);
    assert_eq!(
        app.load.loaded().unwrap().config.monitors[0].value.position,
        "3440x0"
    );

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_display_can_be_switched_off_and_back_on() {
    use crate::edit::MonitorEdit;

    let path = temp_path("monitors-toggle", "hyprland.conf");
    std::fs::write(&path, "decoration:rounding = 4\n").unwrap();
    let mut app = boot_loaded(&path);
    detected(&mut app);

    let _ = app.update(Message::MonitorEdit(
        "DP-3".into(),
        MonitorEdit::Enabled(false, "1920x1080@74.97".into()),
    ));
    assert_eq!(
        app.load.loaded().unwrap().config.monitors[0].value.mode,
        "disable"
    );
    // A disabled card hides its settings but must still render.
    let _ = view::view(&app);

    let _ = app.update(Message::MonitorEdit(
        "DP-3".into(),
        MonitorEdit::Enabled(true, "1920x1080@74.97".into()),
    ));
    assert_eq!(
        app.load.loaded().unwrap().config.monitors[0].value.mode,
        "1920x1080@74.97"
    );

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn rules_with_no_attached_display_stay_visible_and_editable() {
    let path = temp_path("monitors-orphan", "hyprland.conf");
    std::fs::write(
        &path,
        // A catch-all, an EDID selector and an unplugged screen — none of which
        // map to a detected connector.
        "monitor = , preferred, auto, 1\n\
         monitor = desc:Some Vendor XYZ, preferred, auto, 1\n\
         monitor = HDMI-A-9, 1280x1024@60, 0x0, 1\n\
         monitor = DP-1, preferred, 0x0, 1\n",
    )
    .unwrap();
    let mut app = boot_loaded(&path);
    detected(&mut app);

    assert_eq!(app.load.loaded().unwrap().config.monitors.len(), 4);
    // Only DP-1 is attached; the other three must still be reachable.
    let _ = view::view(&app);

    // Expanding a card's advanced block toggles, and re-renders.
    let _ = app.update(Message::ToggleMonitorAdvanced("DP-1".into()));
    assert!(app.expanded_monitors.contains("DP-1"));
    let _ = view::view(&app);
    let _ = app.update(Message::ToggleMonitorAdvanced("DP-1".into()));
    assert!(app.expanded_monitors.is_empty());

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

// ---------------------------------------------------------------------------
// `.conf` -> Lua migration
// ---------------------------------------------------------------------------

#[test]
fn deprecation_banner_shows_for_conf_and_hides_for_lua() {
    let path = temp_path("banner", "hyprland.conf");
    std::fs::write(&path, "decoration:rounding = 4\n").unwrap();
    let mut app = boot_loaded(&path);

    // A `.conf` config is on a deadline, so the banner is shown…
    assert!(app.show_deprecation_banner());
    let _ = view::view(&app);

    // …until dismissed for the session.
    let _ = app.update(Message::DismissDeprecation);
    assert!(!app.show_deprecation_banner());

    // A Lua config never sees it.
    let lua = temp_path("banner-lua", "hyprland.lua");
    std::fs::write(&lua, "hl.config({ decoration = { rounding = 4 } })\n").unwrap();
    let app = boot_loaded(&lua);
    assert!(!app.show_deprecation_banner());

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    let _ = std::fs::remove_dir_all(lua.parent().unwrap());
}

#[test]
fn migration_flow_walks_every_step_and_writes_loadable_lua() {
    let path = temp_path("migrate", "hyprland.conf");
    std::fs::write(
        &path,
        "$mainMod = SUPER\n\
         general {\n\
             col.active_border = rgba(33ccffee) rgba(00ff99ee) 45deg\n\
             gaps_in = 5\n\
         }\n\
         bind = $mainMod, Q, killactive\n\
         bindm = $mainMod, mouse:272, movewindow\n\
         windowrulev2 = float, class:^(pavucontrol)$\n\
         exec-once = true\n",
    )
    .unwrap();
    let mut app = boot_loaded(&path);

    // Opening the flow generates the Lua and kicks off a check.
    let _ = app.update(Message::StartMigration);
    let m = app.migration.as_ref().expect("migration started");
    assert_eq!(m.step, crate::migrate::Step::Review);
    assert_eq!(m.target.extension().unwrap(), "lua");
    // The generated Lua must use the real API, not the `.conf` spellings.
    assert!(m
        .lua
        .contains("hl.bind(\"SUPER + Q\", hl.dsp.window.close())"));
    assert!(m.lua.contains("hl.dsp.window.drag()"));
    assert!(m.lua.contains("float = true"));
    assert!(m.lua.contains("colors = {"));
    assert!(m.lua.contains("hl.on(\"hyprland.start\""));
    assert!(
        !m.lua.contains("\"killactive\""),
        "string dispatchers are rejected by Hyprland"
    );

    // Every step renders.
    for step in crate::migrate::Step::ALL {
        let _ = app.update(Message::MigrateGoto(step));
        let _ = view::view(&app);
    }

    // Real verification, when a Hyprland binary is present.
    let verdict = hyprconf_core::verify::verify_text(&app.migration.as_ref().unwrap().lua, "lua");
    let checked_ok = verdict.is_ok();
    let _ = app.update(Message::MigrateChecked(Box::new(verdict)));
    if checked_ok {
        assert!(app.migration.as_ref().unwrap().can_apply());
    }

    // Write it, then feed the result back as the async task would.
    let m = app.migration.as_ref().unwrap();
    let applied = crate::migrate::apply(m.target.clone(), m.lua.clone());
    let target = m.target.clone();
    let _ = app.update(Message::MigrateApplied(Box::new(applied)));

    let m = app.migration.as_ref().unwrap();
    assert_eq!(m.step, crate::migrate::Step::Done);
    assert!(m.outcome.as_ref().unwrap().is_ok());
    assert!(target.exists(), "the Lua file must exist on disk");
    // The original is kept as a fallback.
    assert!(path.exists(), "the .conf must not be deleted");
    let _ = view::view(&app);

    // The written file re-opens cleanly, so the editor keeps working after
    // migrating.
    let reopened = load::load_config(Some(target.clone()));
    let loaded = match &reopened {
        LoadState::Loaded(l) => l,
        other => panic!("re-opening the migrated file failed: {other:?}"),
    };
    assert_eq!(loaded.format, ConfigFormat::Lua);
    assert_eq!(loaded.config.keybinds.len(), 2);
    assert_eq!(loaded.config.window_rules.len(), 1);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn migration_requires_an_override_when_the_check_fails() {
    use hyprconf_core::verify::{Verdict, VerifyIssue};

    let path = temp_path("migrate-fail", "hyprland.conf");
    std::fs::write(&path, "decoration:rounding = 4\n").unwrap();
    let mut app = boot_loaded(&path);
    let _ = app.update(Message::StartMigration);

    let _ = app.update(Message::MigrateChecked(Box::new(Verdict::Problems(vec![
        VerifyIssue {
            line: Some(3),
            message: "something is wrong".into(),
        },
    ]))));
    assert!(
        !app.migration.as_ref().unwrap().can_apply(),
        "a failed check must block the write"
    );

    let _ = app.update(Message::MigrateGoto(crate::migrate::Step::Check));
    let _ = view::view(&app);

    // …unless the user explicitly opts in.
    let _ = app.update(Message::MigrateOverride(true));
    assert!(app.migration.as_ref().unwrap().can_apply());

    let _ = app.update(Message::CloseMigration);
    assert!(app.migration.is_none());

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// Headless screenshots of real screens, for reviewing layout changes.
///
/// Renders the actual `view()` offscreen (tiny-skia, bundled font) — no
/// window, no compositor. Writes PNGs and never compares, so it is opt-in:
///
/// ```sh
/// HYPRCONF_SHOTS=/tmp/shots cargo test -p hyprconf-gui screenshots -- --ignored
/// ```
///
/// `HYPRCONF_SHOT_CONFIG` renders a specific config (read-only) instead of the
/// built-in demo; `HYPRCONF_SHOT_SIZE=WxH` sets the window size.
#[test]
#[ignore = "writes PNG files; run on demand"]
fn screenshots() {
    let out = PathBuf::from(
        std::env::var("HYPRCONF_SHOTS").unwrap_or_else(|_| "/tmp/hyprconf-shots".into()),
    );
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let (w, h) = std::env::var("HYPRCONF_SHOT_SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1280.0_f32, 860.0_f32));

    let config = std::env::var("HYPRCONF_SHOT_CONFIG")
        .ok()
        .filter(|p| !p.is_empty())
        .map_or_else(
            || {
                let dir = std::env::temp_dir().join("hyprconf-demo").join("hypr");
                std::fs::create_dir_all(&dir).unwrap();
                let path = dir.join("hyprland.conf");
                std::fs::write(&path, DEMO_CONF).unwrap();
                path
            },
            PathBuf::from,
        );
    let mut app = boot_loaded(&config);
    app.window_width = w;
    // The same banner on every shot only costs room.
    let _ = app.update(Message::DismissDeprecation);

    let shoot = |app: &mut App, name: &str, message: Option<Message>| {
        if let Some(m) = message {
            let _ = app.update(m);
        }
        let mut ui = iced_test::simulator::Simulator::with_size(
            iced_test::core::Settings::default(),
            iced::Size::new(w, h),
            app.view(),
        );
        let snapshot = ui.snapshot(&app.theme).expect("render");
        snapshot
            .matches_image(out.join(format!("{name}.png")))
            .expect("write png");
    };

    let section = |id: &str| Some(Message::Selected(Selection::Section(id.into())));
    let collection = |id| Some(Message::Selected(Selection::Collection(id)));
    shoot(&mut app, "01-general", section("general"));
    shoot(&mut app, "02-decoration", section("decoration"));
    shoot(&mut app, "03-input", section("input"));
    shoot(&mut app, "04-group", section("group"));
    shoot(&mut app, "05-keybinds", collection(CollectionId::Keybinds));
    let _ = app.update(Message::ToggleRow(CollectionId::Keybinds, 1));
    shoot(&mut app, "06-keybind-expanded", None);
    shoot(
        &mut app,
        "07-window-rules",
        collection(CollectionId::WindowRules),
    );
    let _ = app.update(Message::ToggleRow(CollectionId::WindowRules, 0));
    shoot(&mut app, "08-window-rule-expanded", None);
    shoot(&mut app, "09-gestures", collection(CollectionId::Gestures));
    let _ = app.update(Message::ToggleRow(CollectionId::Gestures, 0));
    shoot(&mut app, "10-gesture-expanded", None);
    shoot(&mut app, "11-curves", collection(CollectionId::Beziers));
    let _ = app.update(Message::ToggleRow(CollectionId::Beziers, 0));
    shoot(&mut app, "12-curve-expanded", None);
    shoot(
        &mut app,
        "13-animations",
        collection(CollectionId::Animations),
    );
    shoot(&mut app, "14-devices", collection(CollectionId::Devices));
    shoot(&mut app, "15-execs", collection(CollectionId::Execs));
    shoot(
        &mut app,
        "16-permissions",
        collection(CollectionId::Permissions),
    );
    let _ = app.update(Message::Selected(Selection::Section("general".into())));
    let _ = app.update(Message::ToggleGapSides("general:gaps_out".into()));
    shoot(&mut app, "17-gaps-split", None);
    shoot(
        &mut app,
        "18-search",
        Some(Message::SearchChanged("blur".into())),
    );
    let _ = app.update(Message::SearchChanged(String::new()));
    app.window_width = 760.0;
    shoot(&mut app, "19-compact", section("decoration"));
}

/// A representative config for screenshots: every kind of setting and list.
const DEMO_CONF: &str = r"
$mainMod = SUPER
$terminal = kitty

general {
    gaps_in = 4
    gaps_out = 8 12
    border_size = 2
    col.active_border = rgba(33ccffee) rgba(00ff99ee) 45deg
    col.inactive_border = rgba(595959aa)
    layout = dwindle
}
decoration {
    rounding = 10
    active_opacity = 0.95
    blur {
        enabled = true
        size = 6
        passes = 2
    }
    shadow {
        range = 12
        color = rgba(1a1a1aee)
    }
}
input {
    kb_layout = us
    follow_mouse = 1
    sensitivity = -0.2
    touchpad {
        natural_scroll = true
        tap-to-click = true
    }
}
animations {
    enabled = true
}
bezier = easeOutQuint, 0.23, 1, 0.32, 1
bezier = overshot, 0.05, 0.9, 0.1, 1.1
animation = windows, 1, 4.8, easeOutQuint, popin 80%
animation = workspaces, 1, 3, overshot, slide
animation = fade, 0, 3, default

gesture = 3, horizontal, workspace
gesture = 4, down, mod: SUPER, special, magic
device {
    name = epic-mouse-v1
    sensitivity = -0.5
}
permission = /usr/bin/grim, screencopy, allow
plugin = /usr/lib/hyprland/hy3.so

exec-once = waybar
exec-once = hyprpaper
exec = notify-send reloaded
env = XCURSOR_SIZE, 24

monitor = DP-1, 2560x1440@144, 0x0, 1
workspace = 1, monitor:DP-1, default:true
workspace = special:magic, gapsout:40

windowrulev2 = float, class:^(pavucontrol)$
windowrulev2 = opacity 0.9 0.8, class:^(kitty)$
layerrule = blur, waybar

bind = $mainMod, Q, exec, $terminal
bindd = $mainMod, C, Close the window, killactive
bind = $mainMod SHIFT, 1, movetoworkspace, 1
bindel = , XF86AudioRaiseVolume, exec, wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%+
bindm = $mainMod, mouse:272, movewindow
";

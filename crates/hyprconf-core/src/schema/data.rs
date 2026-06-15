// SPDX-License-Identifier: MIT OR Apache-2.0
//! The embedded Hyprland option data, expressed as compact `const` builders.
//!
//! # Provenance
//!
//! - **Option *keys*** are taken verbatim from the vendored upstream stub
//!   `meta/hyprland-config-keys.txt` (Hyprland 0.55.2 `HL.ConfigKey`). The
//!   `schema::tests::every_option_path_exists_in_vendored_stub` test enforces
//!   that every path below maps (via `:` -> `.`) to a real stub key.
//! - **Types, defaults, ranges and descriptions** are taken from the
//!   [Hyprland wiki — Configuring/Variables](https://wiki.hyprland.org/Configuring/Variables/).
//!   Defaults drift between releases; treat them as best-effort and verify with
//!   `hyprctl getoption <name>` on the target version when accuracy is critical.
//! - **`since` hints** are intentionally left `None` for now: the stub carries
//!   no version metadata, so populating them reliably needs a separate wiki
//!   scrape (tracked for a later step).
//!
//! This is a curated, representative subset — broad enough to exercise every
//! section and value kind — not yet the full 341-key surface. Extending it is
//! purely additive: append rows here and the cross-check test keeps them honest.

use super::{
    CollectionId, CollectionSpec, EnumVariant, NumericRange, OptionSpec, Schema, Section, ValueType,
};
use crate::value::{Color, Gradient, Value, Vec2};

/// Build the full embedded schema.
pub(super) fn build() -> Schema {
    Schema::from_parts(
        vec![
            general(),
            decoration(),
            animations(),
            input(),
            gestures(),
            group(),
            misc(),
            binds(),
            layout(),
            dwindle(),
            master(),
            scrolling(),
            xwayland(),
            cursor(),
            opengl(),
            render(),
            ecosystem(),
            debug(),
            experimental(),
            quirks(),
        ],
        collections(),
    )
}

// ---------------------------------------------------------------------------
// terse builders (this file is data, not logic)
// ---------------------------------------------------------------------------

fn sec(id: &str, label: &str, description: &str, options: Vec<OptionSpec>) -> Section {
    Section {
        id: id.to_string(),
        label: label.to_string(),
        description: description.to_string(),
        options,
    }
}

fn spec(
    path: &str,
    label: &str,
    description: &str,
    value_type: ValueType,
    default: Value,
) -> OptionSpec {
    OptionSpec {
        path: path.to_string(),
        label: label.to_string(),
        description: description.to_string(),
        value_type,
        default,
        range: None,
        since: None,
    }
}

fn b(path: &str, label: &str, description: &str, default: bool) -> OptionSpec {
    spec(
        path,
        label,
        description,
        ValueType::Bool,
        Value::Bool(default),
    )
}

fn i(path: &str, label: &str, description: &str, default: i64, range: NumericRange) -> OptionSpec {
    let mut o = spec(
        path,
        label,
        description,
        ValueType::Int,
        Value::Int(default),
    );
    o.range = Some(range);
    o
}

fn fl(path: &str, label: &str, description: &str, default: f64, range: NumericRange) -> OptionSpec {
    let mut o = spec(
        path,
        label,
        description,
        ValueType::Float,
        Value::Float(default),
    );
    o.range = Some(range);
    o
}

fn s(path: &str, label: &str, description: &str, default: &str) -> OptionSpec {
    spec(
        path,
        label,
        description,
        ValueType::String,
        Value::String(default.to_string()),
    )
}

fn c(path: &str, label: &str, description: &str, default: Color) -> OptionSpec {
    spec(
        path,
        label,
        description,
        ValueType::Color,
        Value::Color(default),
    )
}

fn g(path: &str, label: &str, description: &str, default: Gradient) -> OptionSpec {
    spec(
        path,
        label,
        description,
        ValueType::Gradient,
        Value::Gradient(default),
    )
}

fn v(path: &str, label: &str, description: &str, default: Vec2) -> OptionSpec {
    spec(
        path,
        label,
        description,
        ValueType::Vec2,
        Value::Vec2(default),
    )
}

fn e(
    path: &str,
    label: &str,
    description: &str,
    default: &str,
    variants: &[(&str, &str)],
) -> OptionSpec {
    let variants = variants
        .iter()
        .map(|(name, desc)| EnumVariant::described(*name, *desc))
        .collect();
    spec(
        path,
        label,
        description,
        ValueType::Enum(variants),
        Value::Enum(default.to_string()),
    )
}

/// `0xAARRGGBB` -> [`Color`].
const fn argb(v: u32) -> Color {
    Color::rgba(
        ((v >> 16) & 0xff) as u8,
        ((v >> 8) & 0xff) as u8,
        (v & 0xff) as u8,
        ((v >> 24) & 0xff) as u8,
    )
}

/// `0xRRGGBB` -> opaque [`Color`].
const fn rgb(v: u32) -> Color {
    Color::rgba(
        ((v >> 16) & 0xff) as u8,
        ((v >> 8) & 0xff) as u8,
        (v & 0xff) as u8,
        0xff,
    )
}

fn solid(color: Color) -> Gradient {
    Gradient::solid(color)
}

// ---------------------------------------------------------------------------
// sections
// ---------------------------------------------------------------------------

#[rustfmt::skip]
fn general() -> Section {
    sec(
        "general",
        "General",
        "Core layout, gaps and border behaviour.",
        vec![
            i("general:border_size", "Border size", "Window border thickness in px.", 1, NumericRange::at_least(0.0)),
            i("general:gaps_in", "Inner gaps", "Gaps between adjacent windows.", 5, NumericRange::at_least(0.0)),
            i("general:gaps_out", "Outer gaps", "Gaps between windows and screen edges.", 20, NumericRange::at_least(0.0)),
            i("general:gaps_workspaces", "Workspace gaps", "Gaps between workspaces while swiping.", 0, NumericRange::at_least(0.0)),
            g("general:col.active_border", "Active border color", "Border color of the focused window.", solid(argb(0xffffffff))),
            g("general:col.inactive_border", "Inactive border color", "Border color of unfocused windows.", solid(argb(0xff444444))),
            e("general:layout", "Layout", "The tiling layout engine.", "dwindle", &[
                ("dwindle", "BSP-style binary tiling."),
                ("master", "Master/stack tiling."),
                ("scrolling", "PaperWM-style horizontal scrolling tiling."),
                ("monocle", "Monocle: every window fills the whole area."),
            ]),
            b("general:resize_on_border", "Resize on border", "Enable dragging window borders to resize.", false),
            e("general:resize_corner", "Resize corner", "Force floating windows to resize from a fixed corner.", "0", &[
                ("0", "Disabled: resize from the grabbed edge/corner."),
                ("1", "Top-left."),
                ("2", "Top-right."),
                ("3", "Bottom-right."),
                ("4", "Bottom-left."),
            ]),
            i("general:extend_border_grab_area", "Border grab area", "Extra px around borders that respond to drags.", 15, NumericRange::at_least(0.0)),
            b("general:hover_icon_on_border", "Hover icon on border", "Show a resize cursor when hovering a border.", true),
            b("general:allow_tearing", "Allow tearing", "Permit tearing for windows that request it.", false),
            b("general:no_focus_fallback", "No focus fallback", "Do not refocus the last window when the active one closes.", false),
            b("general:snap:enabled", "Snap enabled", "Snap floating windows to edges.", false).since("0.42.0"),
            i("general:snap:window_gap", "Snap window gap", "Distance at which floating windows snap to each other.", 10, NumericRange::at_least(0.0)).since("0.42.0"),
            i("general:snap:monitor_gap", "Snap monitor gap", "Distance at which floating windows snap to monitor edges.", 10, NumericRange::at_least(0.0)).since("0.42.0"),
            b("general:snap:border_overlap", "Snap border overlap", "Allow snapped borders to overlap.", false).since("0.42.0"),
            i("general:float_gaps", "Float gaps", "Gaps between windows and monitor edges for floating windows.", 0, NumericRange::at_least(0.0)),
            g("general:col.nogroup_border", "Nogroup border color", "Inactive border color for window that cannot be added to a group.", solid(argb(0xffffaaff))),
            g("general:col.nogroup_border_active", "Nogroup border active color", "Active border color for window that cannot be added to a group.", solid(argb(0xffff00ff))),
            b("general:snap:respect_gaps", "Snap respect gaps", "If true, snapping will respect gaps between windows.", false),
            b("general:modal_parent_blocking", "Modal parent blocking", "If true, parent windows of modals will not be interactive.", true),
            s("general:locale", "Locale", "Overrides the system locale.", ""),
        ],
    )
}

#[rustfmt::skip]
fn decoration() -> Section {
    sec(
        "decoration",
        "Decoration",
        "Rounding, opacity, blur and shadow.",
        vec![
            i("decoration:rounding", "Rounding", "Corner rounding radius in layout px.", 0, NumericRange::at_least(0.0)),
            fl("decoration:rounding_power", "Rounding power", "Squircle-ness of rounded corners.", 2.0, NumericRange::bounded(1.0, 10.0)).since("0.45.0"),
            fl("decoration:active_opacity", "Active opacity", "Opacity of the focused window.", 1.0, NumericRange::bounded(0.0, 1.0)),
            fl("decoration:inactive_opacity", "Inactive opacity", "Opacity of unfocused windows.", 1.0, NumericRange::bounded(0.0, 1.0)),
            fl("decoration:fullscreen_opacity", "Fullscreen opacity", "Opacity of fullscreen windows.", 1.0, NumericRange::bounded(0.0, 1.0)),
            b("decoration:dim_inactive", "Dim inactive", "Dim windows that are not focused.", false),
            fl("decoration:dim_strength", "Dim strength", "How much to dim inactive windows.", 0.5, NumericRange::bounded(0.0, 1.0)),
            b("decoration:border_part_of_window", "Border part of window", "Count the border as part of the window.", true),
            b("decoration:blur:enabled", "Blur enabled", "Enable background blur.", true),
            i("decoration:blur:size", "Blur size", "Blur kernel size.", 8, NumericRange::at_least(1.0)),
            i("decoration:blur:passes", "Blur passes", "Number of blur passes.", 1, NumericRange::at_least(1.0)),
            b("decoration:blur:new_optimizations", "Blur optimizations", "Enable blur performance optimizations.", true),
            b("decoration:blur:xray", "Blur xray", "Blur behind floating windows as if transparent.", false),
            fl("decoration:blur:noise", "Blur noise", "Noise added to the blur.", 0.0117, NumericRange::bounded(0.0, 1.0)),
            fl("decoration:blur:contrast", "Blur contrast", "Contrast of the blur.", 0.8916, NumericRange::bounded(0.0, 2.0)),
            fl("decoration:blur:brightness", "Blur brightness", "Brightness of the blur.", 0.8172, NumericRange::bounded(0.0, 2.0)),
            fl("decoration:blur:vibrancy", "Blur vibrancy", "Saturation boost of the blur.", 0.1696, NumericRange::bounded(0.0, 1.0)),
            fl("decoration:blur:vibrancy_darkness", "Blur vibrancy darkness", "Vibrancy effect on dark areas.", 0.0, NumericRange::bounded(0.0, 1.0)),
            b("decoration:blur:special", "Blur special", "Blur the special workspace background.", false),
            b("decoration:blur:popups", "Blur popups", "Blur popups (e.g. menus).", false),
            b("decoration:shadow:enabled", "Shadow enabled", "Enable drop shadows.", true),
            i("decoration:shadow:range", "Shadow range", "Shadow size/spread in px.", 4, NumericRange::at_least(0.0)),
            i("decoration:shadow:render_power", "Shadow render power", "Falloff steepness of the shadow.", 3, NumericRange::bounded(1.0, 4.0)),
            c("decoration:shadow:color", "Shadow color", "Drop shadow color.", argb(0xee1a1a1a)),
            v("decoration:shadow:offset", "Shadow offset", "Shadow offset as an x/y vector.", Vec2::new(0.0, 0.0)),
            fl("decoration:shadow:scale", "Shadow scale", "Shadow scale factor.", 1.0, NumericRange::bounded(0.0, 1.0)),
            b("decoration:shadow:sharp", "Shadow sharp", "Render sharp (non-blurred) shadows.", false),
            g("decoration:shadow:color_inactive", "Shadow color inactive", "Inactive shadow color. (if not set, will fall back to col.shadow).", solid(argb(0xee1a1a1a))),
            b("decoration:glow:enabled", "Glow enabled", "Enable inner glow on windows.", false),
            i("decoration:glow:range", "Glow range", "Glow range (size) in layout px.", 10, NumericRange::bounded(0.0, 100.0)),
            i("decoration:glow:render_power", "Glow render power", "In what power to render the falloff (more power, the faster the falloff).", 3, NumericRange::bounded(1.0, 4.0)),
            c("decoration:glow:color", "Glow color", "Glow's color. Alpha dictates glow's opacity.", argb(0xee33ccff)),
            c("decoration:glow:color_inactive", "Glow color inactive", "Inactive glow color. (if not set, will fall back to decoration:glow:color).", argb(0x0033ccff)),
            b("decoration:dim_modal", "Dim modal", "Enables dimming of parents of modal windows.", true),
            fl("decoration:dim_special", "Dim special", "How much to dim the rest of the screen by when a special workspace is open.", 0.2, NumericRange::bounded(0.0, 1.0)),
            fl("decoration:dim_around", "Dim around", "How much the dimaround window rule should dim by.", 0.4, NumericRange::bounded(0.0, 1.0)),
            s("decoration:screen_shader", "Screen shader", "A path to a custom shader to be applied at the end of rendering.", ""),
            b("decoration:blur:ignore_opacity", "Blur ignore opacity", "Make the blur layer ignore the opacity of the window.", true),
            fl("decoration:blur:popups_ignorealpha", "Blur popups ignorealpha", "Works like ignorealpha in layer rules. If pixel opacity is below set value, will not blur.", 0.2, NumericRange::bounded(0.0, 1.0)),
            b("decoration:blur:input_methods", "Blur input methods", "Whether to blur input methods (e.g. fcitx5).", false),
            fl("decoration:blur:input_methods_ignorealpha", "Blur input methods ignorealpha", "Works like ignorealpha in layer rules. If pixel opacity is below set value, will not blur.", 0.2, NumericRange::bounded(0.0, 1.0)),
            b("decoration:motion_blur:enabled", "Motion blur enabled", "Enable motion blur for moving and resizing windows.", false),
            i("decoration:motion_blur:samples", "Motion blur samples", "Amount of samples used for motion blur.", 7, NumericRange::bounded(1.0, 64.0)),
        ],
    )
}

#[rustfmt::skip]
fn animations() -> Section {
    sec(
        "animations",
        "Animations",
        "Global animation toggles. Bezier curves and per-target animations are managed as collections.",
        vec![
            b("animations:enabled", "Enabled", "Master switch for all animations.", true),
            b("animations:workspace_wraparound", "Workspace wraparound", "Animate wrap-around when cycling workspaces.", false),
        ],
    )
}

#[rustfmt::skip]
fn input() -> Section {
    sec(
        "input",
        "Input",
        "Keyboard, mouse and touchpad behaviour.",
        vec![
            s("input:kb_layout", "Keyboard layout", "XKB layout(s), comma-separated.", "us"),
            s("input:kb_variant", "Keyboard variant", "XKB variant(s).", ""),
            s("input:kb_model", "Keyboard model", "XKB model.", ""),
            s("input:kb_options", "Keyboard options", "XKB options, comma-separated.", ""),
            s("input:kb_rules", "Keyboard rules", "XKB rules.", ""),
            e("input:follow_mouse", "Follow mouse", "Specify if and how cursor movement should affect window focus.", "1", &[
                ("0", "Disabled: focus does not follow the mouse."),
                ("1", "Follow: the window under the cursor is focused."),
                ("2", "Detached: clicks are passed to the focused window, focus follows mouse."),
                ("3", "Separate: focus follows mouse, clicks go to the hovered window."),
            ]),
            e("input:focus_on_close", "Focus on close", "Which window to focus when the active window is closed.", "0", &[
                ("0", "Focus the next window candidate."),
                ("1", "Focus the window under the cursor."),
                ("2", "Focus the most recently used window."),
            ]).since("0.45.0"),
            b("input:mouse_refocus", "Mouse refocus", "Refocus the window under the cursor on motion.", true),
            fl("input:sensitivity", "Sensitivity", "Pointer sensitivity (libinput, -1.0 to 1.0).", 0.0, NumericRange::bounded(-1.0, 1.0)),
            e("input:accel_profile", "Accel profile", "Cursor acceleration profile.", "", &[
                ("", "Unset: use the device's libinput default."),
                ("adaptive", "Adaptive acceleration (default for most devices)."),
                ("flat", "Flat: 1:1 movement, no acceleration."),
                ("custom", "Custom curve configured via scroll_points."),
            ]),
            e("input:scroll_method", "Scroll method", "How scrolling is performed for pointer devices.", "", &[
                ("", "Unset: use the device's libinput default."),
                ("2fg", "Two-finger scrolling."),
                ("edge", "Edge scrolling."),
                ("on_button_down", "Scroll while holding the scroll button."),
                ("no_scroll", "Disable scrolling."),
            ]),
            e("input:off_window_axis_events", "Off-window axis events", "How to handle scroll events over the area around a focused window.", "1", &[
                ("0", "Ignore the events."),
                ("1", "Send them to the window."),
                ("2", "Clamp the scroll to the window."),
                ("3", "Warp the cursor onto the window first."),
            ]),
            e("input:emulate_discrete_scroll", "Emulate discrete scroll", "Emulate discrete scrolling from high-resolution scroll events.", "1", &[
                ("0", "Disable emulation."),
                ("1", "Emulate only for non-standard devices."),
                ("2", "Force emulation for all devices."),
            ]),
            b("input:natural_scroll", "Natural scroll", "Invert scroll direction.", false),
            b("input:numlock_by_default", "Numlock by default", "Enable numlock on startup.", false),
            i("input:repeat_rate", "Repeat rate", "Key repeat rate (per second).", 25, NumericRange::at_least(0.0)),
            i("input:repeat_delay", "Repeat delay", "Delay before key repeat begins (ms).", 600, NumericRange::at_least(0.0)),
            b("input:left_handed", "Left handed", "Swap left/right mouse buttons.", false),
            fl("input:scroll_factor", "Scroll factor", "Multiplier for scroll distance.", 1.0, NumericRange::at_least(0.0)),
            b("input:touchpad:natural_scroll", "Touchpad natural scroll", "Invert touchpad scroll direction.", false),
            b("input:touchpad:disable_while_typing", "Disable while typing", "Disable the touchpad while typing.", true),
            b("input:touchpad:tap_to_click", "Tap to click", "Treat a tap as a click.", true),
            fl("input:touchpad:scroll_factor", "Touchpad scroll factor", "Multiplier for touchpad scrolling.", 1.0, NumericRange::at_least(0.0)),
            b("input:touchpad:clickfinger_behavior", "Clickfinger behavior", "Use finger count for click button.", false),
            b("input:touchpad:middle_button_emulation", "Middle button emulation", "Emulate the middle button.", false),
            e("input:touchpad:tap_button_map", "Tap button map", "Which buttons 1/2/3-finger taps emulate.", "", &[
                ("", "Unset: use the device's libinput default."),
                ("lrm", "1/2/3 fingers map to left/right/middle."),
                ("lmr", "1/2/3 fingers map to left/middle/right."),
            ]),
            b("input:touchpad:tap_and_drag", "Tap and drag", "Enable tap-and-drag.", true),
            b("input:touchpad:drag_lock", "Drag lock", "Keep dragging after lifting during tap-and-drag.", false),
            s("input:kb_file", "Kb file", "Appropriate XKB keymap file.", ""),
            b("input:resolve_binds_by_sym", "Resolve binds by sym", "Determines how keybinds act when multiple layouts are used.", false),
            b("input:force_no_accel", "Force no accel", "Force no cursor acceleration.", false),
            i("input:rotation", "Rotation", "Sets the rotation of a device in degrees clockwise. Value is clamped to the range 0 to 359.", 0, NumericRange::bounded(0.0, 359.0)),
            s("input:scroll_points", "Scroll points", "Sets the scroll acceleration profile, when accel_profile is set to custom.", ""),
            i("input:scroll_button", "Scroll button", "Sets the scroll button. 0 means default.", 0, NumericRange::bounded(0.0, 300.0)),
            b("input:scroll_button_lock", "Scroll button lock", "If the scroll button lock is enabled, the button does not need to be held down.", false),
            fl("input:follow_mouse_threshold", "Follow mouse threshold", "The smallest distance in logical pixels the mouse needs to travel for the window under it to get focused.", 0.0, NumericRange::new(None, None, None)),
            i("input:float_switch_override_focus", "Float switch override focus", "If enabled (1 or 2), focus will change to the window under the cursor when changing from tiled-to-floating and vice versa. If 2, focus will also follow mouse on.", 1, NumericRange::bounded(0.0, 2.0)),
            b("input:special_fallthrough", "Special fallthrough", "If enabled, having only floating windows in the special workspace will not block focusing windows in the regular workspace.", false),
            i("input:follow_mouse_shrink", "Follow mouse shrink", "Shrinks the inactive window hitboxes used for focus detection by the specified number of pixels. This creates a dead zone in gaps between windows where moving.", 0, NumericRange::bounded(0.0, 300.0)),
            b("input:touchpad:flip_x", "Touchpad flip x", "Inverts the horizontal movement of the touchpad.", false),
            b("input:touchpad:flip_y", "Touchpad flip y", "Inverts the vertical movement of the touchpad.", false),
            e("input:touchpad:drag_3fg", "Touchpad drag 3fg", "Whether to use 3 or 4 finger drag.", "0", &[
                ("0", "Disable."),
                ("1", "3 finger."),
                ("2", "4 finger."),
            ]),
            i("input:touchdevice:transform", "Touchdevice transform", "Transform the input from touchdevices.", 0, NumericRange::bounded(0.0, 6.0)),
            s("input:touchdevice:output", "Touchdevice output", "The monitor to bind touch devices.", ""),
            b("input:touchdevice:enabled", "Touchdevice enabled", "Whether input is enabled for touch devices.", true),
            e("input:virtualkeyboard:share_states", "Virtualkeyboard share states", "Unify key down states and modifier states with other keyboards.", "2", &[
                ("0", "Disable."),
                ("1", "Enable."),
                ("2", "Only non ime."),
            ]),
            b("input:virtualkeyboard:release_pressed_on_close", "Virtualkeyboard release pressed on close", "Release all pressed keys by virtual keyboard on close.", false),
            i("input:tablet:transform", "Tablet transform", "Transform the input from tablets.", 0, NumericRange::bounded(0.0, 6.0)),
            s("input:tablet:output", "Tablet output", "The monitor to bind tablets.", ""),
            v("input:tablet:region_position", "Tablet region position", "Position of the mapped region in monitor layout.", Vec2::new(0.0, 0.0)),
            b("input:tablet:absolute_region_position", "Tablet absolute region position", "Whether to treat the region_position as an absolute position in monitor layout.", false),
            v("input:tablet:region_size", "Tablet region size", "Size of the mapped region.", Vec2::new(0.0, 0.0)),
            b("input:tablet:relative_input", "Tablet relative input", "Whether the input should be relative.", false),
            b("input:tablet:left_handed", "Tablet left handed", "If enabled, the tablet will be rotated 180 degrees.", false),
            v("input:tablet:active_area_size", "Tablet active area size", "Size of tablet's active area in mm.", Vec2::new(0.0, 0.0)),
            v("input:tablet:active_area_position", "Tablet active area position", "Position of the active area in mm.", Vec2::new(0.0, 0.0)),
            i("input:tablettool:eraser_button_mode", "Tablettool eraser button mode", "Change the eraser button behavior on the tool. When set to 0, use the default hardware behavior of the tool.", 0, NumericRange::bounded(0.0, 6.0)),
            i("input:tablettool:eraser_button_override", "Tablettool eraser button override", "Set a button to be button event when eraser_button_mode is set to 1. Has to be an int, cannot be a string. Must be a valid button (e.g. BTN_STYLUS).", 0, NumericRange::at_least(0.0)),
            fl("input:tablettool:pressure_range_min", "Tablettool pressure range min", "Set the minimum pressure range for the tool, a negative number will set the default minimum pressure value. This is usually 0.0.", -1.0, NumericRange::bounded(-1.0, 1.0)),
            fl("input:tablettool:pressure_range_max", "Tablettool pressure range max", "Set the maximum pressure range for the tool, a negative number will set the default maximum pressure value. This is usually 1.0.", -1.0, NumericRange::bounded(-1.0, 1.0)),
        ],
    )
}

#[rustfmt::skip]
fn gestures() -> Section {
    sec(
        "gestures",
        "Gestures",
        "Touchpad and touch workspace-swipe gestures.",
        vec![
            i("gestures:workspace_swipe_distance", "Swipe distance", "Distance (px) for a full workspace swipe.", 300, NumericRange::at_least(0.0)),
            b("gestures:workspace_swipe_invert", "Swipe invert", "Invert swipe direction.", true),
            fl("gestures:workspace_swipe_cancel_ratio", "Swipe cancel ratio", "Fraction below which a swipe is cancelled.", 0.5, NumericRange::bounded(0.0, 1.0)),
            i("gestures:workspace_swipe_min_speed_to_force", "Min speed to force", "Min speed that forces a workspace change.", 30, NumericRange::at_least(0.0)),
            b("gestures:workspace_swipe_direction_lock", "Direction lock", "Lock swipe to one direction.", true),
            b("gestures:workspace_swipe_create_new", "Create new", "Allow swiping to create a new workspace.", true),
            b("gestures:workspace_swipe_forever", "Swipe forever", "Keep swiping past the last workspace.", false),
            b("gestures:workspace_swipe_touch", "Touch swipe", "Enable workspace swipe via touchscreen.", false),
            b("gestures:workspace_swipe_touch_invert", "Workspace swipe touch invert", "Invert the direction (touchscreen only).", false),
            i("gestures:workspace_swipe_direction_lock_threshold", "Workspace swipe direction lock threshold", "In px, the distance to swipe before direction lock activates.", 10, NumericRange::bounded(0.0, 200.0)),
            b("gestures:workspace_swipe_use_r", "Workspace swipe use r", "If enabled, swiping will use the r prefix instead of the m prefix for finding workspaces.", false),
            i("gestures:close_max_timeout", "Close max timeout", "Timeout for closing windows with the close gesture, in ms.", 1000, NumericRange::bounded(10.0, 2000.0)),
            b("gestures:scrolling:move_snap_to_grid", "Scrolling move snap to grid", "When releasing the scroll move gesture, whether it shoud try to snap to the grid.", true),
            b("gestures:scrolling:move_snap_cursor", "Scrolling move snap cursor", "When releasing the scroll move gesture, whether it shoud snap the cursor to the newly focused window.", true),
        ],
    )
}

#[rustfmt::skip]
fn group() -> Section {
    sec(
        "group",
        "Group",
        "Window grouping and the group bar.",
        vec![
            b("group:auto_group", "Auto group", "Automatically group new windows into the focused group.", true),
            b("group:insert_after_current", "Insert after current", "Insert grouped windows after the active one.", true),
            b("group:focus_removed_window", "Focus removed window", "Focus a window when it leaves a group.", true),
            e("group:drag_into_group", "Drag into group", "Whether dragging a window into an unlocked group merges them.", "1", &[
                ("0", "Disabled: never merge on drag."),
                ("1", "Enabled: merge when dragged into the group."),
                ("2", "Only when dragging into the groupbar."),
            ]),
            b("group:merge_groups_on_drag", "Merge on drag", "Merge groups when dragged onto each other.", true),
            g("group:col.border_active", "Active group border", "Border of the active group.", solid(argb(0x66ffff00))),
            g("group:col.border_inactive", "Inactive group border", "Border of inactive groups.", solid(argb(0x66777700))),
            g("group:col.border_locked_active", "Locked active border", "Border of the active locked group.", solid(argb(0x66ff5500))),
            g("group:col.border_locked_inactive", "Locked inactive border", "Border of inactive locked groups.", solid(argb(0x66775500))),
            b("group:groupbar:enabled", "Groupbar enabled", "Render the group bar.", true),
            i("group:groupbar:font_size", "Groupbar font size", "Group bar title font size.", 8, NumericRange::at_least(1.0)),
            i("group:groupbar:height", "Groupbar height", "Group bar height in px.", 14, NumericRange::at_least(1.0)),
            b("group:groupbar:render_titles", "Render titles", "Show window titles in the group bar.", true),
            g("group:groupbar:col.active", "Groupbar active color", "Active tab color in the group bar.", solid(argb(0x66ffff00))),
            g("group:groupbar:col.inactive", "Groupbar inactive color", "Inactive tab color in the group bar.", solid(argb(0x66777700))),
            b("group:merge_groups_on_groupbar", "Merge groups on groupbar", "Whether one group will be merged with another when dragged into its groupbar.", true),
            b("group:merge_floated_into_tiled_on_groupbar", "Merge floated into tiled on groupbar", "Whether dragging a floating window into a tiled window groupbar will merge them.", false),
            b("group:group_on_movetoworkspace", "Group on movetoworkspace", "Whether using movetoworkspace[silent] will merge the window into the workspace's solitary unlocked group.", false),
            s("group:groupbar:font_family", "Groupbar font family", "Font used to display groupbar titles.", ""),
            e("group:groupbar:font_weight_active", "Groupbar font weight active", "Weight of the font used to display active groupbar titles.", "", &[
                ("", "Inherit the font family's default weight."),
                ("thin", "Thin (100)."),
                ("ultralight", "Ultra-light (200)."),
                ("light", "Light (300)."),
                ("semilight", "Semi-light (350)."),
                ("book", "Book (380)."),
                ("normal", "Normal (400)."),
                ("medium", "Medium (500)."),
                ("semibold", "Semi-bold (600)."),
                ("bold", "Bold (700)."),
                ("ultrabold", "Ultra-bold (800)."),
                ("heavy", "Heavy (900)."),
                ("ultraheavy", "Ultra-heavy (1000)."),
            ]),
            e("group:groupbar:font_weight_inactive", "Groupbar font weight inactive", "Weight of the font used to display inactive groupbar titles.", "", &[
                ("", "Inherit the font family's default weight."),
                ("thin", "Thin (100)."),
                ("ultralight", "Ultra-light (200)."),
                ("light", "Light (300)."),
                ("semilight", "Semi-light (350)."),
                ("book", "Book (380)."),
                ("normal", "Normal (400)."),
                ("medium", "Medium (500)."),
                ("semibold", "Semi-bold (600)."),
                ("bold", "Bold (700)."),
                ("ultrabold", "Ultra-bold (800)."),
                ("heavy", "Heavy (900)."),
                ("ultraheavy", "Ultra-heavy (1000)."),
            ]),
            b("group:groupbar:gradients", "Groupbar gradients", "Enables gradients.", false),
            i("group:groupbar:indicator_gap", "Groupbar indicator gap", "Height of the gap between the groupbar indicator and title.", 0, NumericRange::bounded(0.0, 64.0)),
            i("group:groupbar:indicator_height", "Groupbar indicator height", "Height of the groupbar indicator.", 3, NumericRange::bounded(1.0, 64.0)),
            b("group:groupbar:stacked", "Groupbar stacked", "Render the groupbar as a vertical stack.", false),
            i("group:groupbar:priority", "Groupbar priority", "Sets the decoration priority for groupbars.", 3, NumericRange::bounded(0.0, 6.0)),
            b("group:groupbar:scrolling", "Groupbar scrolling", "Whether scrolling in the groupbar changes group active window.", true),
            b("group:groupbar:middle_click_close", "Groupbar middle click close", "Whether middle clicking the groupbar closes the clicked window.", true),
            i("group:groupbar:rounding", "Groupbar rounding", "How much to round the groupbar.", 1, NumericRange::bounded(0.0, 20.0)),
            fl("group:groupbar:rounding_power", "Groupbar rounding power", "Rounding power of groupbar corners (2 is a circle).", 2.0, NumericRange::bounded(2.0, 10.0)),
            i("group:groupbar:gradient_rounding", "Groupbar gradient rounding", "How much to round the groupbar gradient.", 2, NumericRange::bounded(0.0, 20.0)),
            fl("group:groupbar:gradient_rounding_power", "Groupbar gradient rounding power", "Rounding power of groupbar gradient corners (2 is a circle).", 2.0, NumericRange::bounded(2.0, 10.0)),
            b("group:groupbar:round_only_edges", "Groupbar round only edges", "If yes, will only round at the groupbar edges.", true),
            b("group:groupbar:gradient_round_only_edges", "Groupbar gradient round only edges", "If yes, will only round at the groupbar gradient edges.", true),
            c("group:groupbar:text_color", "Groupbar text color", "Color for window titles in the groupbar.", argb(0xffffffff)),
            c("group:groupbar:text_color_inactive", "Groupbar text color inactive", "Color for inactive windows' titles in the groupbar.", argb(0xffffffff)),
            c("group:groupbar:text_color_locked_active", "Groupbar text color locked active", "Color for the active window's title in a locked group.", argb(0xffffffff)),
            c("group:groupbar:text_color_locked_inactive", "Groupbar text color locked inactive", "Color for inactive windows' titles in locked groups.", argb(0xffffffff)),
            g("group:groupbar:col.locked_active", "Groupbar locked active color", "Active locked group border color.", solid(argb(0x66ff5500))),
            g("group:groupbar:col.locked_inactive", "Groupbar locked inactive color", "Inactive locked group border color.", solid(argb(0x66775500))),
            i("group:groupbar:gaps_out", "Groupbar gaps out", "Gap between gradients and window.", 2, NumericRange::bounded(0.0, 20.0)),
            i("group:groupbar:gaps_in", "Groupbar gaps in", "Gap between gradients.", 2, NumericRange::bounded(0.0, 20.0)),
            b("group:groupbar:keep_upper_gap", "Groupbar keep upper gap", "Keep an upper gap above gradient.", true),
            i("group:groupbar:text_offset", "Groupbar text offset", "Set an offset for a text.", 0, NumericRange::bounded(-20.0, 20.0)),
            i("group:groupbar:text_padding", "Groupbar text padding", "Set horizontal padding for a text.", 0, NumericRange::bounded(0.0, 22.0)),
            b("group:groupbar:blur", "Groupbar blur", "Enable background blur for groupbars.", false),
        ],
    )
}

#[rustfmt::skip]
fn misc() -> Section {
    sec(
        "misc",
        "Misc",
        "Miscellaneous behaviour and cosmetics.",
        vec![
            b("misc:disable_hyprland_logo", "Disable logo", "Hide the Hyprland logo background.", false),
            b("misc:disable_splash_rendering", "Disable splash", "Hide the splash text.", false),
            e("misc:force_default_wallpaper", "Force default wallpaper", "Force one of the built-in default wallpapers.", "-1", &[
                ("-1", "Random: pick one of the defaults at random."),
                ("0", "No wallpaper (solid background)."),
                ("1", "Default wallpaper 1."),
                ("2", "Default wallpaper 2 (anime)."),
            ]),
            e("misc:vrr", "VRR", "Variable refresh rate (adaptive sync) mode.", "0", &[
                ("0", "Off: VRR disabled."),
                ("1", "On: VRR always enabled."),
                ("2", "Fullscreen only: enable VRR for fullscreen windows."),
                ("3", "Fullscreen with a game content-type only."),
            ]),
            b("misc:mouse_move_enables_dpms", "Mouse wakes DPMS", "Wake displays on mouse movement.", false),
            b("misc:key_press_enables_dpms", "Key wakes DPMS", "Wake displays on key press.", false),
            b("misc:always_follow_on_dnd", "Follow on DnD", "Follow the cursor during drag-and-drop.", true),
            b("misc:layers_hog_keyboard_focus", "Layers hog focus", "Let layer surfaces keep keyboard focus.", true),
            b("misc:animate_manual_resizes", "Animate manual resizes", "Animate windows during manual resizes.", false),
            b("misc:animate_mouse_windowdragging", "Animate mouse dragging", "Animate windows during mouse dragging.", false),
            b("misc:focus_on_activate", "Focus on activate", "Focus windows that request activation.", false),
            c("misc:col.splash", "Splash color", "Color of the splash text.", argb(0xffffffff)),
            c("misc:background_color", "Background color", "Solid background color behind windows.", rgb(0x111111)),
            s("misc:font_family", "Font family", "Default font family for built-in text.", "Sans"),
            b("misc:enable_swallow", "Enable swallow", "Enable window swallowing.", false),
            b("misc:middle_click_paste", "Middle-click paste", "Enable primary-selection paste on middle click.", true),
            b("misc:close_special_on_empty", "Close empty special", "Auto-close the special workspace when empty.", true),
            e("misc:on_focus_under_fullscreen", "Focus under fullscreen", "What happens when a tiled window requests focus while a fullscreen/maximized window is present.", "2", &[
                ("0", "Ignore the focus request."),
                ("1", "Take over: the requesting window replaces the fullscreen one."),
                ("2", "Exit fullscreen to focus the requesting window."),
            ]),
            s("misc:splash_font_family", "Splash font family", "Changes the font used to render the splash text.", ""),
            b("misc:name_vk_after_proc", "Name vk after proc", "Name virtual keyboards after the processes that create them.", true),
            b("misc:disable_autoreload", "Disable autoreload", "If true, the config will not reload automatically on save.", false),
            s("misc:swallow_regex", "Swallow regex", "The class regex to be used for windows that should be swallowed.", ""),
            s("misc:swallow_exception_regex", "Swallow exception regex", "The title regex to be used for windows that should not be swallowed.", ""),
            b("misc:mouse_move_focuses_monitor", "Mouse move focuses monitor", "Whether mouse moving into a different monitor should focus it.", true),
            b("misc:allow_session_lock_restore", "Allow session lock restore", "If true, will allow you to restart a lockscreen app in case it crashes.", false),
            b("misc:session_lock_xray", "Session lock xray", "Keep rendering workspaces below your lockscreen.", false),
            b("misc:exit_window_retains_fullscreen", "Exit window retains fullscreen", "If true, closing a fullscreen window makes the next focused window fullscreen.", false),
            i("misc:initial_workspace_tracking", "Initial workspace tracking", "If enabled, windows will open on the workspace they were invoked on.", 1, NumericRange::bounded(0.0, 2.0)),
            i("misc:render_unfocused_fps", "Render unfocused fps", "The maximum limit for renderunfocused windows' fps in the background.", 15, NumericRange::bounded(1.0, 120.0)),
            b("misc:disable_xdg_env_checks", "Disable xdg env checks", "Disable the warning if XDG environment is externally managed.", false),
            b("misc:disable_hyprland_guiutils_check", "Disable hyprland guiutils check", "Disable the warning if hyprland-guiutils is missing.", false),
            b("misc:disable_watchdog_warning", "Disable watchdog warning", "Whether to disable the warning about not using start-hyprland.", false),
            i("misc:lockdead_screen_delay", "Lockdead screen delay", "The delay in ms after the lockdead screen appears.", 1000, NumericRange::bounded(0.0, 5000.0)),
            b("misc:enable_anr_dialog", "Enable anr dialog", "Whether to enable the ANR (app not responding) dialog when your apps hang.", true),
            i("misc:anr_missed_pings", "Anr missed pings", "Number of missed pings before showing the ANR dialog.", 5, NumericRange::bounded(1.0, 20.0)),
            b("misc:screencopy_force_8b", "Screencopy force 8b", "Forces 8 bit screencopy.", true),
            b("misc:disable_scale_notification", "Disable scale notification", "Disables notification popup when a monitor fails to set a suitable scale.", false),
            b("misc:size_limits_tiled", "Size limits tiled", "Whether to apply minsize and maxsize rules to tiled windows.", false),
        ],
    )
}

#[rustfmt::skip]
fn binds() -> Section {
    sec(
        "binds",
        "Binds",
        "Behaviour of keybinds and dispatchers.",
        vec![
            b("binds:workspace_back_and_forth", "Back and forth", "Toggle between current and previous workspace.", false),
            b("binds:allow_workspace_cycles", "Allow cycles", "Allow workspace cycling with back-and-forth.", false),
            b("binds:pass_mouse_when_bound", "Pass mouse when bound", "Pass mouse events even when bound.", false),
            i("binds:scroll_event_delay", "Scroll event delay", "Debounce for scroll-triggered binds (ms).", 300, NumericRange::at_least(0.0)),
            e("binds:focus_preferred_method", "Focus method", "How a directional focus/movewindow target is chosen.", "0", &[
                ("0", "By the longest shared edge."),
                ("1", "By the smallest angle to the cursor/window."),
            ]),
            e("binds:workspace_center_on", "Workspace center on", "What the cursor centers on when switching workspaces.", "1", &[
                ("0", "Center on the workspace."),
                ("1", "Center on the last active window."),
            ]),
            b("binds:movefocus_cycles_fullscreen", "Movefocus cycles fullscreen", "Cycle fullscreen windows with movefocus.", false),
            b("binds:disable_keybind_grabbing", "Disable keybind grabbing", "Disable global keybind grabbing.", false),
            i("binds:drag_threshold", "Drag threshold", "Pixels of motion before a drag begins (0 = instant).", 0, NumericRange::at_least(0.0)),
            b("binds:allow_pin_fullscreen", "Allow pin fullscreen", "Keep pinned windows visible in fullscreen.", false),
            b("binds:hide_special_on_workspace_change", "Hide special on workspace change", "If enabled, changing the active workspace will hide the special workspace on the monitor.", false),
            b("binds:ignore_group_lock", "Ignore group lock", "If enabled, dispatchers like moveintogroup, moveoutofgroup and movewindoworgroup will ignore lock per group.", false),
            b("binds:movefocus_cycles_groupfirst", "Movefocus cycles groupfirst", "If enabled, when in a grouped window, movefocus will cycle windows in the groups first.", false),
            b("binds:window_direction_monitor_fallback", "Window direction monitor fallback", "If enabled, moving a window or focus over the edge of a monitor with a direction will move it to the next monitor.", true),
        ],
    )
}

#[rustfmt::skip]
fn dwindle() -> Section {
    sec(
        "dwindle",
        "Dwindle layout",
        "Options for the dwindle (BSP) layout.",
        vec![
            b("dwindle:preserve_split", "Preserve split", "Keep the split orientation when windows close.", false),
            b("dwindle:smart_split", "Smart split", "Choose split direction from cursor position.", false),
            b("dwindle:smart_resizing", "Smart resizing", "Resize relative to cursor quadrant.", true),
            e("dwindle:force_split", "Force split", "Force a split direction for new windows.", "0", &[
                ("0", "Follow mouse: split toward the cursor."),
                ("1", "Always split to the left/top."),
                ("2", "Always split to the right/bottom."),
            ]),
            e("dwindle:split_bias", "Split bias", "Which window receives the larger share of a new split.", "0", &[
                ("0", "Directional: based on split direction."),
                ("1", "Current: the focused window."),
            ]),
            fl("dwindle:default_split_ratio", "Default split ratio", "Initial split ratio for new windows.", 1.0, NumericRange::bounded(0.1, 1.9)),
            fl("dwindle:split_width_multiplier", "Split width multiplier", "Bias toward horizontal/vertical splits.", 1.0, NumericRange::at_least(0.0)),
            b("dwindle:use_active_for_splits", "Use active for splits", "Split based on the active window.", true),
            fl("dwindle:special_scale_factor", "Special scale factor", "Scale of the special workspace.", 1.0, NumericRange::bounded(0.0, 1.0)),
            b("dwindle:permanent_direction_override", "Permanent direction override", "If enabled, makes the preselect direction persist.", false),
            b("dwindle:precise_mouse_move", "Precise mouse move", "If enabled, bindm movewindow will drop the window more precisely depending on where your mouse is.", false),
        ],
    )
}

#[rustfmt::skip]
fn master() -> Section {
    sec(
        "master",
        "Master layout",
        "Options for the master/stack layout.",
        vec![
            e("master:new_status", "New window status", "Role assigned to new windows.", "slave", &[
                ("master", "Become a new master."),
                ("slave", "Join the stack."),
                ("inherit", "Inherit from the focused window."),
            ]),
            b("master:new_on_top", "New on top", "Add new windows at the top of the stack.", false),
            e("master:new_on_active", "New on active", "Placement of new windows relative to the active one.", "none", &[
                ("before", "Before the active window."),
                ("after", "After the active window."),
                ("none", "Default placement."),
            ]),
            fl("master:mfact", "Master factor", "Fraction of the screen used by the master area.", 0.55, NumericRange::bounded(0.0, 1.0)),
            e("master:orientation", "Orientation", "Position of the master area.", "left", &[
                ("left", "Master on the left."),
                ("right", "Master on the right."),
                ("top", "Master on top."),
                ("bottom", "Master on the bottom."),
                ("center", "Master centered."),
            ]),
            fl("master:special_scale_factor", "Special scale factor", "Scale of the special workspace.", 1.0, NumericRange::bounded(0.0, 1.0)),
            b("master:smart_resizing", "Smart resizing", "Resize relative to cursor quadrant.", true),
            b("master:allow_small_split", "Allow small split", "Allow splitting the master into multiple windows.", false),
            i("master:slave_count_for_center_master", "Slave count for center master", "When using orientation=center, make the master window centered only when at least this many slave windows are open.", 2, NumericRange::bounded(0.0, 10.0)),
            e("master:center_master_fallback", "Center master fallback", "Set fallback for center master when slaves are less than slave_count_for_center_master.", "left", &[
                ("left", "Left."),
                ("right", "Right."),
                ("top", "Top."),
                ("bottom", "Bottom."),
            ]),
            b("master:center_ignores_reserved", "Center ignores reserved", "Centers the master window on monitor ignoring reserved areas.", false),
            b("master:drop_at_cursor", "Drop at cursor", "When enabled, dragging and dropping windows will put them at the cursor position.", true),
            b("master:always_keep_position", "Always keep position", "Whether to keep the master window in its configured position when there are no slave windows.", false),
            b("master:focus_master_on_close", "Focus master on close", "When enabled, closing a window focuses the master window.", false),
        ],
    )
}

#[rustfmt::skip]
fn xwayland() -> Section {
    sec(
        "xwayland",
        "XWayland",
        "X11 compatibility layer.",
        vec![
            b("xwayland:enabled", "Enabled", "Enable XWayland.", true),
            b("xwayland:use_nearest_neighbor", "Nearest neighbor", "Use nearest-neighbor scaling for X11 windows.", true),
            b("xwayland:force_zero_scaling", "Force zero scaling", "Force scale 1 for X11 windows.", false),
            b("xwayland:create_abstract_socket", "Abstract socket", "Create an abstract X11 socket.", false),
        ],
    )
}

#[rustfmt::skip]
fn cursor() -> Section {
    sec(
        "cursor",
        "Cursor",
        "Cursor appearance and behaviour.",
        vec![
            fl("cursor:inactive_timeout", "Inactive timeout", "Seconds before the cursor hides when idle (0 = never).", 0.0, NumericRange::at_least(0.0)),
            e("cursor:no_hardware_cursors", "No hardware cursors", "Disable hardware cursors (forces software cursors).", "2", &[
                ("0", "Disabled: use hardware cursors."),
                ("1", "Enabled: force software cursors."),
                ("2", "Auto: let Hyprland decide."),
            ]),
            b("cursor:enable_hyprcursor", "Enable hyprcursor", "Use the hyprcursor format.", true),
            b("cursor:hide_on_key_press", "Hide on key press", "Hide the cursor when a key is pressed.", false),
            b("cursor:hide_on_touch", "Hide on touch", "Hide the cursor on touchscreen input.", true),
            b("cursor:invisible", "Invisible", "Make the cursor invisible.", false),
            fl("cursor:zoom_factor", "Zoom factor", "Cursor-centered zoom factor.", 1.0, NumericRange::at_least(1.0)),
            s("cursor:default_monitor", "Default monitor", "Monitor the cursor starts on.", ""),
            i("cursor:hotspot_padding", "Hotspot padding", "Padding before the cursor leaves an edge.", 0, NumericRange::at_least(0.0)),
            e("cursor:no_break_fs_vrr", "No break fs vrr", "Disables scheduling new frames on cursor movement for fullscreen apps with VRR enabled.", "2", &[
                ("0", "Disable."),
                ("1", "Enable."),
                ("2", "Auto."),
            ]),
            i("cursor:min_refresh_rate", "Min refresh rate", "Minimum refresh rate for cursor movement when no_break_fs_vrr is active.", 24, NumericRange::bounded(10.0, 500.0)),
            b("cursor:no_warps", "No warps", "If true, will not warp the cursor in many cases.", false),
            b("cursor:persistent_warps", "Persistent warps", "When a window is refocused, the cursor returns to its last position relative to that window.", false),
            e("cursor:warp_on_change_workspace", "Warp on change workspace", "Move the cursor to the last focused window after changing the workspace.", "0", &[
                ("0", "Disable."),
                ("1", "Enable."),
                ("2", "Force."),
            ]),
            e("cursor:warp_on_toggle_special", "Warp on toggle special", "Move the cursor to the last focused window when toggling a special workspace.", "0", &[
                ("0", "Disable."),
                ("1", "Enable."),
                ("2", "Force."),
            ]),
            b("cursor:zoom_rigid", "Zoom rigid", "Whether the zoom should follow the cursor rigidly or loosely.", false),
            b("cursor:zoom_disable_aa", "Zoom disable aa", "If enabled, when zooming, no antialiasing will be used.", false),
            b("cursor:zoom_detached_camera", "Zoom detached camera", "Detaches the camera from the mouse when zoomed in.", true),
            b("cursor:hide_on_tablet", "Hide on tablet", "Hides the cursor when the last input was a tablet input until a mouse input is done.", false),
            e("cursor:use_cpu_buffer", "Use cpu buffer", "Makes HW cursors use a CPU buffer.", "2", &[
                ("0", "Disable."),
                ("1", "Enable."),
                ("2", "Auto."),
            ]),
            b("cursor:sync_gsettings_theme", "Sync gsettings theme", "Sync xcursor theme with gsettings.", true),
            b("cursor:warp_back_after_non_mouse_input", "Warp back after non mouse input", "Warp the cursor back to where it was after using a non-mouse input to move it.", false),
        ],
    )
}

#[rustfmt::skip]
fn render() -> Section {
    sec(
        "render",
        "Render",
        "Low-level rendering options.",
        vec![
            e("render:direct_scanout", "Direct scanout", "Enable direct scanout for fullscreen windows.", "0", &[
                ("0", "Disable direct scanout."),
                ("1", "Enable direct scanout."),
                ("2", "Auto: enable when beneficial."),
            ]),
            b("render:expand_undersized_textures", "Expand undersized textures", "Expand textures smaller than their window.", true),
            e("render:ctm_animation", "CTM animation", "Fade animation for color-transform-matrix (e.g. hyprsunset) changes.", "2", &[
                ("0", "Disable the animation."),
                ("1", "Enable the animation."),
                ("2", "Auto: enable when supported."),
            ]),
            b("render:new_render_scheduling", "New render scheduling", "Use the newer adaptive render scheduler.", false),
            b("render:xp_mode", "Xp mode", "Disable back buffer and bottom layer rendering.", false),
            b("render:cm_enabled", "Cm enabled", "Enable Color Management pipelines (requires restart to fully take effect).", true),
            b("render:send_content_type", "Send content type", "Report content type to allow monitor profile autoswitch.", true),
            e("render:cm_auto_hdr", "Cm auto hdr", "Auto-switch to hdr mode when fullscreen app is in hdr.", "1", &[
                ("0", "Disable."),
                ("1", "Hdr."),
                ("2", "Hdredid."),
            ]),
            e("render:non_shader_cm", "Non shader cm", "Enable CM without shader.", "3", &[
                ("0", "Disable."),
                ("1", "Always."),
                ("2", "Ondemand."),
                ("3", "Ignore."),
            ]),
            s("render:cm_sdr_eotf", "Cm sdr eotf", "Default transfer function for displaying SDR apps.", "default"),
            b("render:commit_timing_enabled", "Commit timing enabled", "Enable commit timing proto. Requires restart.", true),
            b("render:icc_vcgt_enabled", "Icc vcgt enabled", "Enable sending VCGT ramps to KMS with ICC profiles.", true),
            b("render:use_shader_blur_blend", "Use shader blur blend", "Use experimental blurred bg blending.", false),
            e("render:use_fp16", "Use fp16", "Use experimental internal FP16 buffer.", "2", &[
                ("0", "Disable."),
                ("1", "Enable."),
                ("2", "Auto."),
            ]),
            e("render:keep_unmodified_copy", "Keep unmodified copy", "Keep umodified SDR frame copy for sreensharing.", "2", &[
                ("0", "Disable."),
                ("1", "Enable."),
                ("2", "Auto."),
            ]),
            e("render:non_shader_cm_interop", "Non shader cm interop", "Non_shader_cm interaction with ctm proto (hyprsunset and similar).", "2", &[
                ("0", "Disable."),
                ("1", "Enable."),
                ("2", "Auto."),
            ]),
            e("render:fp16_sdr_tf", "Fp16 sdr tf", "Internal workbuffer transfer function for fp16 in SDR mode.", "0", &[
                ("0", "Monitor."),
                ("1", "Linear."),
            ]),
        ],
    )
}

#[rustfmt::skip]
fn debug() -> Section {
    sec(
        "debug",
        "Debug",
        "Diagnostics and logging. Usually left at defaults.",
        vec![
            b("debug:overlay", "Overlay", "Show the FPS/debug overlay.", false),
            b("debug:damage_blink", "Damage blink", "Flash damaged regions (epilepsy warning).", false),
            b("debug:disable_logs", "Disable logs", "Disable logging.", true),
            b("debug:disable_time", "Disable time", "Omit timestamps from logs.", true),
            b("debug:enable_stdout_logs", "Stdout logs", "Also log to stdout.", false),
            e("debug:damage_tracking", "Damage tracking", "How much of the display to redraw each frame.", "2", &[
                ("0", "None: always redraw everything."),
                ("1", "Monitor: redraw whole monitors that changed."),
                ("2", "Full: redraw only the changed regions."),
            ]),
            b("debug:disable_scale_checks", "Disable scale checks", "Skip fractional-scale sanity checks.", false),
            b("debug:vfr", "VFR", "Variable frame rate (render only on changes).", true),
            b("debug:gl_debugging", "Gl debugging", "Enable OpenGL debugging and error checking.", false),
            i("debug:manual_crash", "Manual crash", "Set to 1 and then back to 0 to crash Hyprland.", 0, NumericRange::bounded(0.0, 1.0)),
            b("debug:suppress_errors", "Suppress errors", "If true, do not display config file parsing errors.", false),
            i("debug:error_limit", "Error limit", "Limits the number of displayed config file parsing errors.", 5, NumericRange::bounded(0.0, 20.0)),
            e("debug:error_position", "Error position", "Sets the position of the error bar.", "0", &[
                ("0", "Top."),
                ("1", "Bottom."),
            ]),
            b("debug:colored_stdout_logs", "Colored stdout logs", "Enables colors in the stdout logs.", true),
            b("debug:log_damage", "Log damage", "Enables logging the damage.", false),
            b("debug:pass", "Pass", "Enables render pass debugging.", false),
            b("debug:full_cm_proto", "Full cm proto", "Claims support for all cm proto features (requires restart).", false),
            b("debug:ds_handle_same_buffer", "Ds handle same buffer", "Special case for DS with unmodified buffer.", true),
            b("debug:ds_handle_same_buffer_fifo", "Ds handle same buffer fifo", "Special case for DS with unmodified buffer unlocks fifo.", true),
            b("debug:fifo_pending_workaround", "Fifo pending workaround", "Fifo workaround for empty pending list.", false),
            b("debug:render_solitary_wo_damage", "Render solitary wo damage", "Render solitary window with empty damage.", false),
            e("debug:invalidate_fp16", "Invalidate fp16", "Allow fp16 buffer invalidation.", "1", &[
                ("0", "Disable."),
                ("1", "Enable."),
                ("2", "Auto."),
            ]),
        ],
    )
}

#[rustfmt::skip]
fn opengl() -> Section {
    sec(
        "opengl",
        "OpenGL",
        "OpenGL renderer tweaks.",
        vec![
            b("opengl:nvidia_anti_flicker", "Nvidia anti flicker", "Reduces flickering on nvidia at the cost of possible frame drops on lower-end GPUs.", true),
        ],
    )
}

#[rustfmt::skip]
fn ecosystem() -> Section {
    sec(
        "ecosystem",
        "Ecosystem",
        "Hyprland ecosystem popups and permission control.",
        vec![
            b("ecosystem:no_update_news", "No update news", "Disable the popup that shows up when you update hyprland to a new version.", false),
            b("ecosystem:no_donation_nag", "No donation nag", "Disable the popup that shows up twice a year encouraging to donate.", false),
            b("ecosystem:enforce_permissions", "Enforce permissions", "Whether to enable permission control.", false),
        ],
    )
}

#[rustfmt::skip]
fn layout() -> Section {
    sec(
        "layout",
        "Layout",
        "Cross-layout options shared by all layouts.",
        vec![
            v("layout:single_window_aspect_ratio", "Single window aspect ratio", "If specified, whenever only a single window is open, it will be coerced into the specified aspect ratio.", Vec2::new(0.0, 0.0)),
            fl("layout:single_window_aspect_ratio_tolerance", "Single window aspect ratio tolerance", "Minimum distance for single_window_aspect_ratio to take effect.", 0.1, NumericRange::bounded(0.0, 1.0)),
        ],
    )
}

#[rustfmt::skip]
fn scrolling() -> Section {
    sec(
        "scrolling",
        "Scrolling layout",
        "Options for the scrolling (PaperWM-style) layout.",
        vec![
            b("scrolling:fullscreen_on_one_column", "Fullscreen on one column", "When enabled, a single column on a workspace will always span the entire screen.", true),
            fl("scrolling:column_width", "Column width", "The default width of a column.", 0.5, NumericRange::bounded(0.1, 1.0)),
            e("scrolling:focus_fit_method", "Focus fit method", "When a column is focused, what method should be used to bring it into view.", "1", &[
                ("0", "Center."),
                ("1", "Fit."),
            ]),
            b("scrolling:follow_focus", "Follow focus", "When a window is focused, should the layout move to bring it into view automatically.", true),
            fl("scrolling:follow_min_visible", "Follow min visible", "When a window is focused, require that at least a given fraction of it is visible for focus to follow.", 0.4, NumericRange::bounded(0.0, 1.0)),
            s("scrolling:explicit_column_widths", "Explicit column widths", "A comma-separated list of preconfigured widths for colresize +conf/-conf.", "0.333, 0.5, 0.667, 1.0"),
            e("scrolling:direction", "Direction", "Direction in which new windows appear and the layout scrolls.", "right", &[
                ("right", "Right."),
                ("left", "Left."),
                ("up", "Up."),
                ("down", "Down."),
            ]),
            b("scrolling:wrap_focus", "Wrap focus", "Determines if column focus wraps around.", true),
            b("scrolling:wrap_swapcol", "Wrap swapcol", "Determines if column movement wraps around.", true),
        ],
    )
}

#[rustfmt::skip]
fn experimental() -> Section {
    sec(
        "experimental",
        "Experimental",
        "Experimental, unstable features. Use with caution.",
        vec![
            b("experimental:wp_cm_1_2", "Wp cm 1 2", "Allow wp-cm-v1 version 2.", false),
        ],
    )
}

#[rustfmt::skip]
fn quirks() -> Section {
    sec(
        "quirks",
        "Quirks",
        "Hardware and driver workarounds.",
        vec![
            e("quirks:prefer_hdr", "Prefer hdr", "Prefer HDR mode.", "0", &[
                ("0", "Disable."),
                ("1", "Enable."),
                ("2", "Gamescope only."),
            ]),
            b("quirks:skip_non_kms_dmabuf_formats", "Skip non kms dmabuf formats", "Do not report dmabuf formats which cannot be imported into KMS.", false),
        ],
    )
}

// ---------------------------------------------------------------------------
// structured collections
// ---------------------------------------------------------------------------

fn collection(
    id: CollectionId,
    label: &str,
    description: &str,
    element_type: ValueType,
    keywords: &[&str],
) -> CollectionSpec {
    CollectionSpec {
        id,
        label: label.to_string(),
        description: description.to_string(),
        element_type,
        keywords: keywords.iter().map(|k| (*k).to_string()).collect(),
        since: None,
    }
}

#[rustfmt::skip]
fn collections() -> Vec<CollectionSpec> {
    vec![
        collection(CollectionId::Monitors, "Monitors", "Per-output resolution, position, scale and transforms.", ValueType::MonitorRule, &["monitor"]),
        collection(CollectionId::Workspaces, "Workspaces", "Persistent/per-monitor workspace rules.", ValueType::Workspace, &["workspace"]),
        collection(CollectionId::WindowRules, "Window rules", "Per-window behaviour and appearance rules.", ValueType::WindowRule, &["windowrule", "windowrulev2"]),
        collection(CollectionId::LayerRules, "Layer rules", "Rules for layer-shell surfaces (bars, notifications).", ValueType::LayerRule, &["layerrule"]),
        collection(
            CollectionId::Keybinds,
            "Keybinds",
            "Key and mouse bindings with their flag variants.",
            ValueType::Keybind,
            &["bind", "bindm", "binde", "bindr", "bindl", "bindel", "bindn", "bindt", "bindi"],
        ),
        collection(CollectionId::Submaps, "Submaps", "Named bind scopes (modal keymaps).", ValueType::Submap, &["submap"]),
        collection(CollectionId::Env, "Environment", "Environment variables exported to the session.", ValueType::EnvVar, &["env", "envd"]),
        collection(CollectionId::Execs, "Exec", "Commands run on launch/reload/shutdown.", ValueType::Exec, &["exec", "exec-once", "exec-shutdown"]),
        collection(CollectionId::Variables, "Variables", "hyprlang `$variables` (textual macros).", ValueType::Variable, &[]),
        collection(CollectionId::Beziers, "Bezier curves", "Named bezier curves used by animations.", ValueType::Bezier, &["bezier"]),
        collection(CollectionId::Animations, "Animation rules", "Per-target animation settings.", ValueType::Animation, &["animation"]),
    ]
}

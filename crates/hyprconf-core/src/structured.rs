// SPDX-License-Identifier: MIT OR Apache-2.0
//! Structured, repeatable configuration constructs.
//!
//! Unlike scalar [`crate::value::Value`]s — which are keyed by a single dotted
//! path — these directives are *ordered collections*: a config has a list of
//! keybinds, a list of window rules, a list of monitors, and so on. Their order
//! is semantically meaningful (rules are applied top-to-bottom, variables must
//! be defined before use), so the [`crate::model::Config`] stores them in
//! `Vec`s rather than a map.
//!
//! In this step these types only need to *exist* and be constructible; the
//! parsers and serializers that populate them arrive in later steps.

use crate::value::Vec2;

/// The flag letters that may be appended to a `bind` keyword (`bindel`, `bindm`,
/// ...). Each corresponds to a documented Hyprland bind modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeybindFlags {
    /// `l` — works while the session is locked.
    pub locked: bool,
    /// `r` — fires on key release instead of press.
    pub release: bool,
    /// `e` — repeats while held.
    pub repeat: bool,
    /// `n` — non-consuming (event passes through to the focused client).
    pub non_consuming: bool,
    /// `m` — mouse bind (`bindm`).
    pub mouse: bool,
    /// `t` — transparent (does not block other binds).
    pub transparent: bool,
    /// `i` — ignores modifier state.
    pub ignore_mods: bool,
    /// `o` — fires on a long press.
    pub long_press: bool,
    /// `c` — fires on a click (press and release without movement).
    pub click: bool,
    /// `g` — fires on a drag.
    pub drag: bool,
    /// `p` — bypasses apps that inhibit keybinds (e.g. games, VMs).
    pub dont_inhibit: bool,
}

/// Reads one flag off a [`KeybindFlags`].
pub type FlagGetter = fn(&KeybindFlags) -> bool;

impl KeybindFlags {
    /// Every flag, with its `.conf` letter — the single table the parser,
    /// the serializers and the GUI all share.
    pub const LETTERS: [(char, FlagGetter); 11] = [
        ('m', |f| f.mouse),
        ('l', |f| f.locked),
        ('r', |f| f.release),
        ('e', |f| f.repeat),
        ('n', |f| f.non_consuming),
        ('i', |f| f.ignore_mods),
        ('t', |f| f.transparent),
        ('o', |f| f.long_press),
        ('c', |f| f.click),
        ('g', |f| f.drag),
        ('p', |f| f.dont_inhibit),
    ];

    /// Parse the flag letters after `bind` (unknown letters are ignored; the
    /// `d` description letter is handled by the caller because it changes the
    /// argument layout).
    #[must_use]
    pub fn from_letters(letters: &str) -> Self {
        let has = |c: char| letters.contains(c);
        Self {
            locked: has('l'),
            release: has('r'),
            repeat: has('e'),
            non_consuming: has('n'),
            mouse: has('m'),
            transparent: has('t'),
            ignore_mods: has('i'),
            long_press: has('o'),
            click: has('c'),
            drag: has('g'),
            dont_inhibit: has('p'),
        }
    }

    /// The canonical `bind` keyword for these flags (e.g. `binde`, `bindml`).
    ///
    /// Flag letters are emitted in Hyprland's conventional order so output is
    /// deterministic.
    #[must_use]
    pub fn keyword(&self) -> String {
        let mut kw = String::from("bind");
        for (letter, enabled) in Self::LETTERS {
            if enabled(self) {
                kw.push(letter);
            }
        }
        kw
    }
}

/// A single key/mouse binding: `bind<flags> = MODS, KEY, DISPATCHER, ARGS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keybind {
    /// Which `bind*` keyword variant produced this entry.
    pub flags: KeybindFlags,
    /// The raw modifier expression, e.g. `SUPER SHIFT` or `$mainMod`.
    pub mods: String,
    /// The key or button, e.g. `Q`, `code:24`, `mouse:272`.
    pub key: String,
    /// The dispatcher name, e.g. `exec`, `killactive`, `movefocus`.
    pub dispatcher: String,
    /// Dispatcher arguments (may be empty).
    pub args: String,
    /// The submap this bind belongs to (`None` = the global/default submap).
    pub submap: Option<String>,
    /// A human description (`bindd = MODS, KEY, DESCRIPTION, ...` in `.conf`,
    /// the `description` option in Lua). Shown by `hyprctl binds` and helper
    /// tools that list your shortcuts.
    pub description: Option<String>,
}

impl Keybind {
    /// The `.conf` keyword: the flag letters, plus `d` when a description is
    /// present (which inserts the description as the third field).
    #[must_use]
    pub fn keyword(&self) -> String {
        let mut kw = self.flags.keyword();
        if self.description.is_some() {
            kw.push('d');
        }
        kw
    }
}

/// A gesture binding:
/// `gesture[p] = FINGERS, DIRECTION[, mod: MODS][, scale: F], ACTION[, ARGS…]`.
///
/// Fields keep the `.conf` spellings (`cursorZoom`, `scrollMove`); the Lua
/// side translates them to `hl.gesture({ action = "cursor_zoom", … })`.
#[derive(Debug, Clone, PartialEq)]
pub struct Gesture {
    /// Number of fingers (2–9).
    pub fingers: u32,
    /// One of [`GESTURE_DIRECTIONS`].
    pub direction: String,
    /// Required modifiers (empty = none).
    pub mods: String,
    /// Animation scale multiplier, if any.
    pub scale: Option<f64>,
    /// The action (see [`GESTURE_ACTIONS`]).
    pub action: String,
    /// Action arguments, comma-separated as in `.conf` (`scratchpad`,
    /// `maximize`, `1.2, mult`, `workspace, e+1`).
    pub args: String,
    /// `gesturep`: fire even while an app inhibits shortcuts.
    pub bypass_inhibit: bool,
}

/// Swipe/pinch directions Hyprland accepts for gestures.
pub const GESTURE_DIRECTIONS: &[(&str, &str)] = &[
    ("horizontal", "Swipe left or right"),
    ("vertical", "Swipe up or down"),
    ("left", "Swipe left"),
    ("right", "Swipe right"),
    ("up", "Swipe up"),
    ("down", "Swipe down"),
    ("swipe", "Swipe in any direction"),
    ("pinch", "Pinch in or out"),
    ("pinchin", "Pinch in"),
    ("pinchout", "Pinch out"),
];

/// Gesture actions (in their `.conf` spelling) with what their arguments mean.
pub const GESTURE_ACTIONS: &[(&str, &str)] = &[
    ("workspace", "Switch workspaces (no arguments)"),
    ("move", "Move the window (no arguments)"),
    ("resize", "Resize the window (no arguments)"),
    ("close", "Close the window (no arguments)"),
    ("special", "Toggle a special workspace — argument: its name"),
    (
        "fullscreen",
        "Toggle fullscreen — optional argument: maximize",
    ),
    (
        "float",
        "Toggle floating — optional argument: float or tile",
    ),
    (
        "cursorZoom",
        "Zoom around the cursor — arguments: factor[, mult|live]",
    ),
    ("scrollMove", "Scroll the scrolling layout (no arguments)"),
    (
        "dispatcher",
        "Run a dispatcher when the gesture ends — arguments: dispatcher, params",
    ),
    ("unset", "Remove a previously bound gesture"),
];

impl Gesture {
    /// The `.conf` keyword (`gesture`, or `gesturep` to bypass inhibitors).
    #[must_use]
    pub fn keyword(&self) -> &'static str {
        if self.bypass_inhibit {
            "gesturep"
        } else {
            "gesture"
        }
    }

    /// Parse the right-hand side of a `gesture[p] = …` line.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason when the finger count, direction or
    /// action is missing or malformed.
    pub fn parse(args: &str, bypass_inhibit: bool) -> Result<Self, String> {
        let mut parts = args.split(',').map(str::trim);
        let fingers = parts
            .next()
            .and_then(|f| f.parse::<u32>().ok())
            .ok_or("expected a finger count first")?;
        let direction = parts
            .next()
            .filter(|d| !d.is_empty())
            .ok_or("expected a direction after the finger count")?
            .to_string();

        let mut mods = String::new();
        let mut scale = None;
        let mut rest: Vec<&str> = Vec::new();
        for part in parts {
            if rest.is_empty() {
                if let Some(m) = part.strip_prefix("mod:") {
                    mods = m.trim().to_string();
                    continue;
                }
                if let Some(s) = part.strip_prefix("scale:") {
                    scale = Some(s.trim().parse::<f64>().map_err(|_| "invalid scale")?);
                    continue;
                }
            }
            rest.push(part);
        }
        let action = rest
            .first()
            .filter(|a| !a.is_empty())
            .ok_or("expected an action")?
            .to_string();
        Ok(Self {
            fingers,
            direction,
            mods,
            scale,
            action,
            args: rest[1..].join(", "),
            bypass_inhibit,
        })
    }

    /// The gesture as Hyprland spells it after `gesture = `.
    #[must_use]
    pub fn to_keyword_value(&self) -> String {
        let mut fields = vec![self.fingers.to_string(), self.direction.clone()];
        if !self.mods.trim().is_empty() {
            fields.push(format!("mod: {}", self.mods.trim()));
        }
        if let Some(scale) = self.scale {
            fields.push(format!("scale: {scale}"));
        }
        fields.push(self.action.clone());
        if !self.args.trim().is_empty() {
            fields.push(self.args.trim().to_string());
        }
        fields.join(", ")
    }
}

/// A per-device override block: `device { name = …; option = value … }`.
///
/// `options` holds `(field, value)` pairs in source order, keyed by their
/// canonical (Lua) field names — `tap_to_click`, not `tap-to-click`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The device name as `hyprctl devices` reports it (e.g. `epic-mouse-v1`).
    pub name: String,
    /// The overridden options, in order.
    pub options: Vec<(String, String)>,
}

/// The options a `device { … }` block may set, with a short explanation.
pub const DEVICE_OPTIONS: &[(&str, &str)] = &[
    ("sensitivity", "Pointer speed, -1.0 to 1.0"),
    ("accel_profile", "adaptive, flat or custom"),
    ("natural_scroll", "Invert scrolling (true/false)"),
    ("left_handed", "Swap the buttons (true/false)"),
    ("scroll_factor", "Scroll distance multiplier"),
    ("scroll_method", "2fg, edge, on_button_down or no_scroll"),
    ("scroll_button", "Button used for on_button_down scrolling"),
    ("scroll_button_lock", "Toggle instead of hold (true/false)"),
    ("scroll_points", "Custom acceleration points"),
    (
        "middle_button_emulation",
        "Left+right click = middle (true/false)",
    ),
    (
        "disable_while_typing",
        "Touchpad off while typing (true/false)",
    ),
    ("tap_to_click", "Tap to click (true/false)"),
    ("tap_and_drag", "Tap and drag (true/false)"),
    ("drag_lock", "0 off, 1 timeout, 2 sticky"),
    (
        "drag_3fg",
        "Three-finger drag: 0 off, 1 three, 2 four fingers",
    ),
    ("tap_button_map", "lrm or lmr"),
    (
        "clickfinger_behavior",
        "Button by finger count (true/false)",
    ),
    ("flip_x", "Invert horizontal movement (true/false)"),
    ("flip_y", "Invert vertical movement (true/false)"),
    ("kb_layout", "Keyboard layout(s)"),
    ("kb_variant", "Keyboard variant(s)"),
    ("kb_model", "Keyboard model"),
    ("kb_options", "XKB options"),
    ("kb_rules", "XKB rules"),
    ("kb_file", "Path to an XKB keymap file"),
    ("repeat_rate", "Key repeats per second"),
    ("repeat_delay", "Delay before repeating, in ms"),
    ("numlock_by_default", "Numlock on at startup (true/false)"),
    (
        "resolve_binds_by_sym",
        "Resolve binds by keysym (true/false)",
    ),
    ("share_states", "Virtual keyboards: 0, 1 or 2"),
    (
        "release_pressed_on_close",
        "Release keys on close (true/false)",
    ),
    ("output", "Monitor to map a tablet/touch device to"),
    ("transform", "Rotation of the input, 0–7"),
    ("rotation", "Rotation in degrees"),
    ("region_position", "Tablet mapped region position (x y)"),
    ("region_size", "Tablet mapped region size (w h)"),
    (
        "absolute_region_position",
        "Region position is absolute (true/false)",
    ),
    ("relative_input", "Relative tablet input (true/false)"),
    (
        "active_area_position",
        "Tablet active area position, mm (x y)",
    ),
    ("active_area_size", "Tablet active area size, mm (w h)"),
    ("enabled", "Whether the device is enabled (true/false)"),
    (
        "keybinds",
        "Whether this device triggers keybinds (true/false)",
    ),
    ("tags", "Comma-separated tags for bind device filters"),
];

/// A permission rule: `permission = BINARY_REGEX, TYPE, MODE`.
///
/// Rules only take effect with `ecosystem:enforce_permissions` enabled, and
/// Hyprland must be restarted to apply changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permission {
    /// A regex matching the binary path (or, for `keyboard`, a device name).
    pub binary: String,
    /// One of [`PERMISSION_TYPES`].
    pub kind: String,
    /// One of [`PERMISSION_MODES`].
    pub mode: String,
}

/// The permission types Hyprland understands.
pub const PERMISSION_TYPES: &[(&str, &str)] = &[
    ("screencopy", "Capture the screen"),
    ("plugin", "Load a plugin"),
    ("keyboard", "Use a keyboard (matches the device name)"),
    ("cursorpos", "Read the cursor position"),
    ("input-capture", "Capture input (remote desktop, KVM)"),
];

/// What a permission rule does.
pub const PERMISSION_MODES: &[(&str, &str)] = &[
    ("allow", "Always allow"),
    ("ask", "Ask every time"),
    ("deny", "Always deny"),
];

/// A plugin to load at startup: `plugin = /path/to/plugin.so`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plugin {
    /// Absolute path to the plugin's shared object.
    pub path: String,
}

/// A window rule: legacy `windowrule = RULE, REGEX` or `windowrulev2 = RULE, MATCHERS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowRule {
    /// `true` for `windowrulev2`, `false` for legacy `windowrule`.
    pub v2: bool,
    /// The rule body, e.g. `float`, `opacity 0.9`, `workspace 2 silent`.
    pub rule: String,
    /// The matcher text: a window regex (v1) or `key:value` matchers (v2).
    pub matchers: String,
}

/// A layer-surface rule: `layerrule = RULE, NAMESPACE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerRule {
    /// The rule body, e.g. `blur`, `ignorezero`.
    pub rule: String,
    /// The target layer namespace, e.g. `waybar`, `^(notifications)$`.
    pub namespace: String,
}

/// A monitor directive: `monitor = NAME, MODE, POSITION, SCALE[, extra...]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorRule {
    /// Connector name or selector (`DP-1`, `desc:...`, `` (empty) or `*`).
    pub name: String,
    /// Resolution/refresh, e.g. `1920x1080@144`, `preferred`, `highres`, `disable`.
    pub mode: String,
    /// Position, e.g. `0x0`, `auto`, `auto-right`.
    pub position: String,
    /// Scale factor, e.g. `1`, `1.5`, `auto`.
    pub scale: String,
    /// Trailing modifiers (`transform`, `mirror`, `bitdepth`, `vrr`, ...).
    pub extra: Vec<String>,
}

impl MonitorRule {
    /// The rule as Hyprland spells it after `monitor = `, i.e.
    /// `NAME, MODE, POSITION, SCALE[, extra…]`.
    ///
    /// Shared by the `.conf` serializer and by live `hyprctl keyword monitor`
    /// application, so the file and the running compositor can never disagree
    /// about formatting.
    #[must_use]
    pub fn to_keyword_value(&self) -> String {
        let mut fields = vec![
            self.name.clone(),
            self.mode.clone(),
            self.position.clone(),
            self.scale.clone(),
        ];
        fields.extend(self.extra.iter().cloned());
        fields.join(", ")
    }

    /// Whether this rule turns its output off (`mode` is `disable`).
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self.mode.trim().eq_ignore_ascii_case("disable")
    }
}

/// A workspace rule: `workspace = SELECTOR, RULES`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRule {
    /// The workspace selector, e.g. `1`, `name:web`, `special:magic`.
    pub selector: String,
    /// The comma-separated rule list, e.g. `monitor:DP-1, default:true`.
    pub rules: String,
}

/// An environment variable directive: `env = NAME, VALUE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvVar {
    /// The variable name.
    pub name: String,
    /// The variable value.
    pub value: String,
}

/// Which flavour of `exec` directive a command uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecKind {
    /// `exec` — run on every config (re)load.
    #[default]
    Exec,
    /// `exec-once` — run once, at startup.
    ExecOnce,
    /// `exec-shutdown` — run when Hyprland exits.
    ExecShutdown,
}

/// An exec directive: `exec` / `exec-once` / `exec-shutdown = COMMAND`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exec {
    /// Which exec flavour this is.
    pub kind: ExecKind,
    /// The shell command to run.
    pub command: String,
}

/// A bezier curve definition: `bezier = NAME, X0, Y0, X1, Y1`.
#[derive(Debug, Clone, PartialEq)]
pub struct Bezier {
    /// The curve's name (referenced by [`Animation::curve`]).
    pub name: String,
    /// First control point.
    pub p0: Vec2,
    /// Second control point.
    pub p1: Vec2,
}

/// An animation directive: `animation = NAME, ONOFF, SPEED, CURVE[, STYLE]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    /// The animation target name, e.g. `windows`, `workspaces`, `global`.
    pub name: String,
    /// Whether the animation is enabled (`ONOFF`).
    pub enabled: bool,
    /// Speed in deciseconds.
    pub speed: f64,
    /// The bezier curve name to use.
    pub curve: String,
    /// Optional style argument (e.g. `slide`, `popin 80%`).
    pub style: Option<String>,
}

/// A submap marker: `submap = NAME` (or `submap = reset`).
///
/// Binds that follow a submap declaration belong to it until the next
/// `submap = reset`; that association is reconstructed by the parser and is
/// also recorded on [`Keybind::submap`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submap {
    /// The submap name (`reset` is represented by the literal name `reset`).
    pub name: String,
}

/// A hyprlang variable definition: `$NAME = VALUE`.
///
/// Variables are textual macros expanded before evaluation. They have no
/// dedicated value type; the value is stored verbatim for faithful round-trips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    /// The variable name *without* the leading `$`.
    pub name: String,
    /// The raw replacement text.
    pub value: String,
}

/// A uniform wrapper over every structured construct.
///
/// The typed [`crate::model::Config`] collections are preferred for editing;
/// this enum exists for code that needs to handle any structured item
/// generically (e.g. a future generic serializer dispatch).
#[derive(Debug, Clone, PartialEq)]
pub enum StructuredValue {
    /// See [`Keybind`].
    Keybind(Keybind),
    /// See [`WindowRule`].
    WindowRule(WindowRule),
    /// See [`LayerRule`].
    LayerRule(LayerRule),
    /// See [`MonitorRule`].
    MonitorRule(MonitorRule),
    /// See [`WorkspaceRule`].
    Workspace(WorkspaceRule),
    /// See [`EnvVar`].
    EnvVar(EnvVar),
    /// See [`Exec`].
    Exec(Exec),
    /// See [`Bezier`].
    Bezier(Bezier),
    /// See [`Animation`].
    Animation(Animation),
    /// See [`Submap`].
    Submap(Submap),
    /// See [`Variable`].
    Variable(Variable),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keybind_keyword_is_plain_for_no_flags() {
        assert_eq!(KeybindFlags::default().keyword(), "bind");
    }

    #[test]
    fn keybind_keyword_combines_flags_in_order() {
        let flags = KeybindFlags {
            repeat: true,
            locked: true,
            ..Default::default()
        };
        assert_eq!(flags.keyword(), "bindle");

        let mouse = KeybindFlags {
            mouse: true,
            ..Default::default()
        };
        assert_eq!(mouse.keyword(), "bindm");
    }

    #[test]
    fn new_bind_flags_round_trip_through_letters() {
        let flags = KeybindFlags::from_letters("ocgp");
        assert!(flags.long_press && flags.click && flags.drag && flags.dont_inhibit);
        assert_eq!(flags.keyword(), "bindocgp");
        assert_eq!(KeybindFlags::from_letters(&flags.keyword()[4..]), flags);
    }

    #[test]
    fn gestures_parse_modifiers_and_arguments() {
        let g = Gesture::parse(
            "4, down, mod: SUPER, scale: 1.5, special, scratchpad",
            false,
        )
        .unwrap();
        assert_eq!(g.fingers, 4);
        assert_eq!(g.direction, "down");
        assert_eq!(g.mods, "SUPER");
        assert_eq!(g.scale, Some(1.5));
        assert_eq!(g.action, "special");
        assert_eq!(g.args, "scratchpad");
        assert_eq!(
            g.to_keyword_value(),
            "4, down, mod: SUPER, scale: 1.5, special, scratchpad"
        );

        let zoom = Gesture::parse("2, pinchin, cursorZoom, 1.2, mult", true).unwrap();
        assert_eq!(zoom.args, "1.2, mult");
        assert_eq!(zoom.keyword(), "gesturep");

        assert!(Gesture::parse("3", false).is_err());
        assert!(Gesture::parse("x, left, close", false).is_err());
        assert!(Gesture::parse("3, left", false).is_err());
    }

    #[test]
    fn exec_kind_defaults_to_exec() {
        assert_eq!(ExecKind::default(), ExecKind::Exec);
    }

    #[test]
    fn structured_value_is_constructible() {
        let v = StructuredValue::EnvVar(EnvVar {
            name: "XCURSOR_SIZE".into(),
            value: "24".into(),
        });
        assert!(matches!(v, StructuredValue::EnvVar(_)));
    }
}

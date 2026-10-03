// SPDX-License-Identifier: MIT OR Apache-2.0
//! End-to-end checks that the Lua we generate is accepted by the **real**
//! Hyprland binary.
//!
//! Unit tests can only assert that the output matches what we *believe* the API
//! to be. These assert what Hyprland actually accepts, which is what caught the
//! original conversion bugs: `.conf`-style bind chords (`"SUPER, Q"`), string
//! dispatchers, and `.conf`-style gradient strings all round-tripped happily
//! through our own code while being rejected at load.
//!
//! The tests skip themselves when no Hyprland binary is installed, so CI and
//! non-Hyprland dev machines stay green.

use hyprconf_core::conf::{bundle_to_config, ConfParser};
use hyprconf_core::lua::LuaSerializer;
use hyprconf_core::schema::Schema;
use hyprconf_core::verify::{self, Verdict};

/// Convert a `.conf` fixture to Lua and hand it to Hyprland for validation.
fn convert_and_verify(conf: &str) -> Option<Verdict> {
    if !verify::available() {
        eprintln!("skipping: no Hyprland binary on PATH");
        return None;
    }
    let dir = std::env::temp_dir().join(format!(
        "hyprconf-it-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("in.conf");
    std::fs::write(&path, conf).unwrap();

    let bundle = ConfParser::parse_file(&path).expect("parse conf");
    let (config, _) = bundle_to_config(&bundle, Schema::shared());
    let lua = LuaSerializer::serialize(&config);

    let verdict = verify::verify_text(&lua, "lua");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);

    if let Verdict::Unavailable(reason) = &verdict {
        eprintln!("skipping: {reason}");
        return None;
    }
    Some(verdict)
}

fn assert_converts_cleanly(conf: &str) {
    let Some(verdict) = convert_and_verify(conf) else {
        return;
    };
    assert!(
        verdict.is_ok(),
        "Hyprland rejected the generated Lua:\n{:#?}",
        verdict.issues()
    );
}

#[test]
fn keybinds_convert_to_loadable_lua() {
    // Every one of these failed before the rewrite: the chord separator, the
    // string dispatcher, and the mouse-bind form.
    assert_converts_cleanly(
        "$mainMod = SUPER\n\
         bind = $mainMod, Q, killactive\n\
         bind = $mainMod SHIFT, E, exit\n\
         bind = SUPER_ALT, S, exec, kitty\n\
         bind = , XF86AudioMute, exec, wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle\n\
         bindm = $mainMod, mouse:272, movewindow\n\
         bindm = $mainMod, mouse:273, resizewindow\n\
         binde = $mainMod, right, resizeactive, 10 0\n\
         bind = $mainMod, 1, workspace, 1\n\
         bind = $mainMod SHIFT, 1, movetoworkspacesilent, 1\n\
         bind = $mainMod, S, togglespecialworkspace, magic\n\
         bind = $mainMod, F, fullscreen, 1\n\
         bind = $mainMod, V, togglefloating\n\
         bind = $mainMod, J, togglesplit\n\
         bind = $mainMod, left, movefocus, l\n",
    );
}

#[test]
fn gradients_convert_to_loadable_lua() {
    // `.conf` writes a gradient as one string; Lua's colour parser rejects that
    // and needs `{ colors = { ... }, angle = N }`.
    assert_converts_cleanly(
        "general {\n\
         \x20   col.active_border = rgba(33ccffee) rgba(00ff99ee) 45deg\n\
         \x20   col.inactive_border = rgba(595959aa)\n\
         }\n",
    );
}

#[test]
fn window_and_layer_rules_convert_to_loadable_lua() {
    assert_converts_cleanly(
        "windowrulev2 = float, class:^(pavucontrol)$\n\
         windowrulev2 = opacity 0.9 0.9, class:^(kitty)$\n\
         windowrulev2 = size 800 600, class:^(foo)$\n\
         windowrulev2 = suppressevent maximize, class:.*\n\
         windowrulev2 = noanim, class:^(bar)$\n\
         windowrule = float, ^(legacy)$\n\
         layerrule = blur, waybar\n\
         layerrule = ignorezero, notifications\n",
    );
}

#[test]
fn animations_and_curves_convert_to_loadable_lua() {
    // The positional `hl.curve(name, x0, y0, x1, y1)` / `hl.animation(name, ...)`
    // forms are rejected outright by 0.56.
    assert_converts_cleanly(
        "bezier = easeOutQuint, 0.23, 1, 0.32, 1\n\
         bezier = linear, 0, 0, 1, 1\n\
         animations {\n\
         \x20   enabled = yes, please :)\n\
         \x20   animation = windows, 1, 4.79, easeOutQuint\n\
         \x20   animation = windowsOut, 1, 1.49, linear, popin 87%\n\
         }\n",
    );
}

#[test]
fn submaps_and_autostart_convert_to_loadable_lua() {
    assert_converts_cleanly(
        "exec-once = true\n\
         exec = true\n\
         env = XCURSOR_SIZE, 24\n\
         submap = resize\n\
         binde = , right, resizeactive, 10 0\n\
         bind = , escape, submap, reset\n\
         submap = reset\n",
    );
}

#[test]
fn monitors_and_workspace_rules_convert_to_loadable_lua() {
    assert_converts_cleanly(
        "monitor = , preferred, auto, 1\n\
         monitor = DP-1, 1920x1080@144, 0x0, 1, transform, 1\n\
         workspace = 2, monitor:DP-1, default:true\n\
         workspace = w[tv1], gapsout:0, gapsin:0\n",
    );
}

#[test]
fn the_stock_hyprland_conf_converts_cleanly() {
    // A condensed version of the config Hyprland ships, exercising the
    // block-directive forms real 0.56 configs use (`windowrule { }`,
    // `device { }`, `gesture = ...`) which must not leak into `hl.config`.
    assert_converts_cleanly(
        "$mainMod = SUPER\n\
         monitor = , preferred, auto, auto\n\
         general {\n\
         \x20   gaps_in = 5\n\
         \x20   border_size = 2\n\
         \x20   col.active_border = rgba(33ccffee) rgba(00ff99ee) 45deg\n\
         \x20   layout = dwindle\n\
         }\n\
         cursor {\n\
         \x20   no_hardware_cursors = true\n\
         }\n\
         input {\n\
         \x20   kb_layout = us\n\
         \x20   follow_mouse = 1\n\
         \x20   touchpad {\n\
         \x20       natural_scroll = false\n\
         \x20   }\n\
         }\n\
         gesture = 3, horizontal, workspace\n\
         device {\n\
         \x20   name = epic-mouse-v1\n\
         \x20   sensitivity = -0.5\n\
         }\n\
         windowrule {\n\
         \x20   name = suppress-maximize\n\
         \x20   match:class = .*\n\
         \x20   suppress_event = maximize\n\
         }\n\
         bind = $mainMod, Q, killactive\n",
    );
}

/// `windowrule { … }` blocks are the form Hyprland's own generated configs use
/// since 0.56. A line-oriented parser sees them as *sections*, which quietly
/// turned every rule into a bogus `windowrule:float` option — the rules vanished
/// while the generated file still reported "config ok".
#[test]
fn block_form_window_rules_survive_conversion() {
    let conf = "windowrule {\n\
                \x20   name = float-it\n\
                \x20   match:class = ^(pavucontrol)$\n\
                \x20   float = yes\n\
                \x20   size = 1100 800\n\
                \x20   center = yes\n\
                }\n\
                windowrule {\n\
                \x20   name = suppress\n\
                \x20   match:class = .*\n\
                \x20   suppress_event = maximize\n\
                }\n\
                layerrule {\n\
                \x20   match:namespace = ^(waybar)$\n\
                \x20   blur = true\n\
                }\n";

    // The rules must reach the model at all…
    let dir = std::env::temp_dir().join(format!("hyprconf-blocks-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("in.conf");
    std::fs::write(&path, conf).unwrap();
    let bundle = ConfParser::parse_file(&path).expect("parse conf");
    let (config, _) = bundle_to_config(&bundle, Schema::shared());
    assert_eq!(
        config.window_rules.len(),
        4,
        "each rule field in a block is its own .conf rule"
    );
    assert_eq!(config.layer_rules.len(), 1);

    let lua = LuaSerializer::serialize(&config);
    // …and be merged back into one call per original block.
    assert_eq!(lua.matches("hl.window_rule(").count(), 2);
    // `float = yes` is a hyprlang boolean; Lua demands a real `true`.
    assert!(lua.contains("float = true"), "got:\n{lua}");
    assert!(lua.contains("size = \"1100 800\""), "got:\n{lua}");
    assert!(lua.contains("suppress_event = \"maximize\""), "got:\n{lua}");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);

    assert_converts_cleanly(conf);
}

/// `gesture` is repeatable, so parsing it as a scalar option silently kept only
/// the last one. `device { … }` is a block, not a settings section.
#[test]
fn gestures_and_devices_convert_to_their_own_calls() {
    let conf = "gesture = 3, horizontal, workspace\n\
                gesture = 4, vertical, close\n\
                device {\n\
                \x20   name = epic-mouse-v1\n\
                \x20   sensitivity = -0.5\n\
                }\n";
    let dir = std::env::temp_dir().join(format!("hyprconf-gest-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("in.conf");
    std::fs::write(&path, conf).unwrap();
    let bundle = ConfParser::parse_file(&path).expect("parse conf");
    let (config, _) = bundle_to_config(&bundle, Schema::shared());
    let lua = LuaSerializer::serialize(&config);

    assert_eq!(lua.matches("hl.gesture(").count(), 2, "got:\n{lua}");
    assert!(
        lua.contains(
            "hl.gesture({ fingers = 3, direction = \"horizontal\", action = \"workspace\" })"
        ),
        "got:\n{lua}"
    );
    assert!(
        lua.contains("hl.device({ name = \"epic-mouse-v1\", sensitivity = -0.5 })"),
        "got:\n{lua}"
    );
    // They must not leak into `hl.config`, which rejects unknown keys.
    assert!(!lua.contains("gesture = {"), "got:\n{lua}");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);

    assert_converts_cleanly(conf);
}

#[test]
fn unmapped_dispatchers_still_produce_a_loadable_config() {
    // A plugin dispatcher has no `hl.dsp.*` constructor. The fallback shim must
    // keep the whole file loadable rather than failing the migration.
    assert_converts_cleanly(
        "bind = SUPER, X, someplugin:dosomething, with args\n\
         bind = SUPER, Y, killactive\n",
    );
}

/// The constructs that used to be flattened into bogus `gesture:*`/`device:*`
/// options (or not modelled at all) must convert to Lua the compositor loads.
#[test]
fn gestures_devices_permissions_plugins_and_gaps_convert_to_loadable_lua() {
    assert_converts_cleanly(
        "general {\n    gaps_in = 5 10\n    gaps_out = 4 8 12 16\n}\n\
         input {\n    touchpad {\n        tap-to-click = false\n        drag_lock = 2\n    }\n}\n\
         input-capture {\n    capture_modifiers = true\n}\n\
         cursor {\n    no_hardware_cursors = true\n}\n\
         gesture = 3, horizontal, workspace\n\
         gesture = 4, down, mod: SUPER, special, magic\n\
         gesturep = 2, pinchin, cursorZoom, 1.5, mult\n\
         gesture = 4, up, dispatcher, workspace, e+1\n\
         device {\n    name = epic-mouse\n    sensitivity = -0.5\n    tap-to-click = false\n}\n\
         permission = /usr/bin/grim, screencopy, allow\n\
         bindd = SUPER, Q, Close the window, killactive\n\
         bindo = SUPER, W, exec, kitty\n",
    );
}

/// The regenerated `.conf` must load too: `tap-to-click` and `input-capture`
/// are spelled differently there, and Hyprland rejects the Lua spellings.
#[test]
fn regenerated_conf_uses_the_conf_spellings() {
    if !verify::available() {
        eprintln!("skipping: no Hyprland binary on PATH");
        return;
    }
    let doc = ConfParser::parse_str(
        "input {\n    touchpad {\n        tap-to-click = false\n        tap-and-drag = false\n    }\n}\n\
         input-capture {\n    enforce_barriers = false\n}\n\
         general {\n    gaps_out = 5 10\n}\n",
        None,
    );
    let (config, _) = hyprconf_core::conf::document_to_config(&doc, Schema::shared());
    let text = hyprconf_core::conf::config_to_conf(&config);
    match verify::verify_text(&text, "conf") {
        Verdict::Unavailable(reason) => eprintln!("skipping: {reason}"),
        verdict => assert!(
            verdict.is_ok(),
            "Hyprland rejected:\n{text}\n{:#?}",
            verdict.issues()
        ),
    }
}

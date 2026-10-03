// SPDX-License-Identifier: MIT OR Apache-2.0
//! Translation between hyprlang dispatcher names and the Lua `hl.dsp.*` API.
//!
//! # Why this exists
//!
//! In `.conf`, a bind's action is a *string*: `bind = SUPER, Q, killactive`.
//! In Lua it must be a real `HL.Dispatcher` object built by calling one of the
//! `hl.dsp.*` constructors. Hyprland enforces this strictly — passing a string
//! fails at load with:
//!
//! ```text
//! hl.bind: dispatcher must be a dispatcher (e.g. hl.dsp.window.close())
//!          or a lua function
//! ```
//!
//! and there is deliberately **no** generic "dispatch by name" escape hatch
//! (`hl.dispatch("togglesplit")` is rejected the same way). A converter
//! therefore cannot punt: every dispatcher it emits has to be a real call with
//! the right argument *shape*, because Hyprland also validates those
//! (`hl.window.tag: expected a table { tag, window? }`).
//!
//! The tables below were derived by probing Hyprland 0.56.1 with
//! `Hyprland --verify-config`; see `docs/lua-api.md` for the transcript.
//!
//! # Fallback
//!
//! Exotic or plugin-provided dispatchers that have no `hl.dsp.*` equivalent are
//! emitted as [`Fidelity::Shim`] — `hl.dsp.exec_raw("hyprctl dispatch ...")`.
//! That is functionally correct (hyprctl still speaks the legacy names) but
//! spawns a process, so it is reported to the UI and surfaced in the migration
//! report for the user to hand-tune.

use crate::lua::emit::{call, Expr, Table};

/// How faithfully a conf dispatcher could be expressed in the Lua API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fidelity {
    /// A direct, native `hl.dsp.*` equivalent.
    Native,
    /// Translated, but the arguments could not be fully understood; the
    /// remainder was passed through verbatim and deserves a look.
    Approximate,
    /// No native equivalent: emitted as a `hyprctl dispatch` shim.
    Shim,
}

/// The result of translating a conf dispatcher into Lua.
#[derive(Debug, Clone, PartialEq)]
pub struct Translated {
    /// The rendered Lua expression, e.g. `hl.dsp.window.close()`.
    pub lua: String,
    /// How faithful the translation is.
    pub fidelity: Fidelity,
    /// A human-readable note when `fidelity` is not [`Fidelity::Native`].
    pub note: Option<String>,
}

impl Translated {
    fn native(lua: String) -> Self {
        Self {
            lua,
            fidelity: Fidelity::Native,
            note: None,
        }
    }

    fn approximate(lua: String, note: impl Into<String>) -> Self {
        Self {
            lua,
            fidelity: Fidelity::Approximate,
            note: Some(note.into()),
        }
    }
}

/// Translate a hyprlang dispatcher + argument string into a Lua `hl.dsp.*` call.
///
/// Never fails: unknown dispatchers fall back to a `hyprctl dispatch` shim so a
/// converted config always loads.
#[must_use]
pub fn to_lua(dispatcher: &str, args: &str) -> Translated {
    let d = dispatcher.trim();
    let a = args.trim();
    translate(d, a).unwrap_or_else(|| shim(d, a))
}

/// A `hyprctl dispatch <name> <args>` fallback for dispatchers with no native
/// Lua constructor (plugin dispatchers, rarely-used legacy verbs).
fn shim(dispatcher: &str, args: &str) -> Translated {
    let cmd = if args.is_empty() {
        format!("hyprctl dispatch {dispatcher}")
    } else {
        format!("hyprctl dispatch {dispatcher} {args}")
    };
    Translated {
        lua: call("hl.dsp.exec_raw", &[Expr::str(cmd)]),
        fidelity: Fidelity::Shim,
        note: Some(format!(
            "`{dispatcher}` has no native Lua equivalent; kept working via a \
             `hyprctl dispatch` shim. Consider replacing it with an `hl.dsp.*` \
             call or a Lua function."
        )),
    }
}

/// Build `hl.dsp.<path>(<table>)`, or `hl.dsp.<path>()` when the table is empty.
fn dsp_table(path: &str, table: Table) -> String {
    if table.is_empty() {
        format!("hl.dsp.{path}()")
    } else {
        format!("hl.dsp.{path}({})", table.render_inline())
    }
}

/// Build `hl.dsp.<path>(<args...>)`.
fn dsp(path: &str, args: &[Expr]) -> String {
    call(&format!("hl.dsp.{path}"), args)
}

/// Expand Hyprland's single-letter direction shorthands.
fn direction(token: &str) -> Option<&'static str> {
    match token.trim().to_ascii_lowercase().as_str() {
        "l" | "left" => Some("left"),
        "r" | "right" => Some("right"),
        "u" | "up" => Some("up"),
        "d" | "down" => Some("down"),
        _ => None,
    }
}

/// Split `"10 0"` into two whitespace-separated tokens.
fn two(args: &str) -> Option<(&str, &str)> {
    let mut it = args.split_whitespace();
    let a = it.next()?;
    let b = it.next()?;
    if it.next().is_some() {
        return None;
    }
    Some((a, b))
}

/// A window/monitor/workspace selector argument, inferred to its natural type.
fn sel(text: &str) -> Expr {
    Expr::infer(text)
}

#[allow(clippy::too_many_lines)]
fn translate(d: &str, a: &str) -> Option<Translated> {
    let empty = a.is_empty();
    let t = |lua: String| Some(Translated::native(lua));

    match d {
        // ---- execution -------------------------------------------------
        "exec" => t(dsp("exec_cmd", &[Expr::str(a)])),
        "execr" => t(dsp("exec_raw", &[Expr::str(a)])),

        // ---- lifecycle -------------------------------------------------
        "exit" => t(dsp("exit", &[])),
        "forcerendererreload" => t(dsp("force_renderer_reload", &[])),
        "forceidle" => {
            let n = a.parse::<f64>().unwrap_or(1.0);
            t(dsp("force_idle", &[Expr::num(n)]))
        }
        "global" => t(dsp("global", &[Expr::str(a)])),
        "event" => t(dsp("event", &[Expr::str(a)])),
        "submap" => t(dsp("submap", &[Expr::str(a)])),
        "layoutmsg" => t(dsp("layout", &[Expr::str(a)])),
        "togglesplit" | "swapsplit" | "preselect" => {
            let msg = if empty {
                d.to_string()
            } else {
                format!("{d} {a}")
            };
            t(dsp("layout", &[Expr::str(msg)]))
        }
        "dpms" => {
            // `dpms on|off|toggle [monitor]`
            let mut it = a.split_whitespace();
            let action = it.next().unwrap_or("toggle");
            let monitor = it.next();
            match monitor {
                None => t(dsp("dpms", &[Expr::str(action)])),
                Some(m) => t(dsp("dpms", &[Expr::str(action), Expr::str(m)])),
            }
        }

        // ---- closing / killing ----------------------------------------
        "killactive" => t(dsp("window.close", &[])),
        "forcekillactive" => t(dsp("window.kill", &[])),
        "closewindow" => {
            let mut tb = Table::new();
            tb.set_str_opt("window", a);
            t(dsp_table("window.close", tb))
        }
        "killwindow" => {
            let mut tb = Table::new();
            tb.set_str_opt("window", a);
            t(dsp_table("window.kill", tb))
        }

        // ---- floating / tiling ----------------------------------------
        "togglefloating" | "setfloating" | "settiled" => {
            let action = match d {
                "setfloating" => "set",
                "settiled" => "unset",
                _ => "toggle",
            };
            let mut tb = Table::new();
            tb.set_str("action", action);
            // `togglefloating active` is the implicit default; anything else is
            // a window selector.
            if !empty && a != "active" {
                tb.set("window", sel(a));
            }
            t(dsp_table("window.float", tb))
        }
        "pseudo" => t(dsp("window.pseudo", &[])),
        "pin" => {
            let mut tb = Table::new();
            tb.set_str_opt("window", a);
            t(dsp_table("window.pin", tb))
        }
        "centerwindow" => {
            // `centerwindow 1` means "respect reserved area".
            if empty {
                t(dsp("window.center", &[]))
            } else {
                t(dsp("window.center", &[Expr::infer(a)]))
            }
        }

        // ---- fullscreen ------------------------------------------------
        "fullscreen" => {
            // 0 = fullscreen, 1 = maximized, 2 = fullscreen w/o sending the
            // state to the client.
            let mut tb = Table::new();
            match a {
                "" | "0" => {}
                "1" => {
                    tb.set_str("mode", "maximized");
                }
                "2" => {
                    return Some(Translated::approximate(
                        dsp_table("window.fullscreen_state", {
                            let mut s = Table::new();
                            s.set("internal", Expr::Int(2));
                            s.set("client", Expr::Int(0));
                            s
                        }),
                        "`fullscreen 2` maps to a fullscreen_state pair; verify \
                         the client/internal split matches what you expect.",
                    ));
                }
                other => {
                    tb.set_str("mode", other);
                }
            }
            t(dsp_table("window.fullscreen", tb))
        }
        "fullscreenstate" => {
            let mut tb = Table::new();
            let mut it = a.split_whitespace();
            if let Some(i) = it.next() {
                tb.set("internal", Expr::infer(i));
            }
            if let Some(c) = it.next() {
                tb.set("client", Expr::infer(c));
            }
            t(dsp_table("window.fullscreen_state", tb))
        }

        // ---- focus -----------------------------------------------------
        "movefocus" => {
            let dir = direction(a)?;
            let mut tb = Table::new();
            tb.set_str("direction", dir);
            t(dsp_table("focus", tb))
        }
        "workspace" => {
            let mut tb = Table::new();
            tb.set("workspace", sel(a));
            t(dsp_table("focus", tb))
        }
        "focuswindow" => {
            let mut tb = Table::new();
            tb.set("window", sel(a));
            t(dsp_table("focus", tb))
        }
        "focusmonitor" => {
            let mut tb = Table::new();
            tb.set("monitor", sel(a));
            t(dsp_table("focus", tb))
        }
        "focusurgentorlast" => {
            let mut tb = Table::new();
            tb.set("urgent_or_last", Expr::Bool(true));
            t(dsp_table("focus", tb))
        }
        "focuscurrentorlast" => {
            let mut tb = Table::new();
            tb.set("current_or_last", Expr::Bool(true));
            t(dsp_table("focus", tb))
        }
        "cyclenext" => {
            let mut tb = Table::new();
            for token in a.split_whitespace() {
                match token {
                    "prev" | "last" => {
                        tb.set("previous", Expr::Bool(true));
                    }
                    "tiled" => {
                        tb.set("tiled", Expr::Bool(true));
                    }
                    "floating" => {
                        tb.set("floating", Expr::Bool(true));
                    }
                    "visible" => {
                        tb.set("visible", Expr::Bool(true));
                    }
                    _ => {}
                }
            }
            t(dsp_table("window.cycle_next", tb))
        }

        // ---- moving windows --------------------------------------------
        "movewindow" => {
            let mut tb = Table::new();
            match (direction(a), a.strip_prefix("mon:")) {
                (Some(dir), _) => tb.set_str("direction", dir),
                (None, Some(mon)) => tb.set("monitor", sel(mon)),
                // A bare `movewindow` with no target is the mouse-drag form,
                // handled by `to_lua_mouse`.
                (None, None) => return None,
            };
            t(dsp_table("window.move", tb))
        }
        "swapwindow" => {
            let dir = direction(a)?;
            let mut tb = Table::new();
            tb.set_str("direction", dir);
            t(dsp_table("window.swap", tb))
        }
        "movetoworkspace" | "movetoworkspacesilent" => {
            let mut tb = Table::new();
            tb.set("workspace", sel(a));
            if d.ends_with("silent") {
                tb.set("silent", Expr::Bool(true));
            }
            t(dsp_table("window.move", tb))
        }
        "moveactive" | "movewindowpixel" => {
            let (x, y, window) = pixel_args(a, d == "movewindowpixel")?;
            let mut tb = Table::new();
            tb.set("x", Expr::infer(&x)).set("y", Expr::infer(&y));
            if let Some(w) = window {
                tb.set("window", sel(&w));
            }
            t(dsp_table("window.move", tb))
        }
        "resizeactive" | "resizewindowpixel" => {
            let (x, y, window) = pixel_args(a, d == "resizewindowpixel")?;
            let mut tb = Table::new();
            tb.set("x", Expr::infer(&x)).set("y", Expr::infer(&y));
            if let Some(w) = window {
                tb.set("window", sel(&w));
            }
            t(dsp_table("window.resize", tb))
        }
        "alterzorder" => {
            let mut it = a.splitn(2, ',');
            let mode = it.next()?.trim();
            let mut tb = Table::new();
            tb.set_str("mode", mode);
            if let Some(w) = it.next() {
                tb.set("window", sel(w.trim()));
            }
            t(dsp_table("window.alter_zorder", tb))
        }
        "bringactivetotop" => t(dsp("window.bring_to_top", &[])),
        "toggleswallow" => t(dsp("window.toggle_swallow", &[])),
        "tagwindow" => {
            // `tagwindow [+-]tag [window]`
            let mut it = a.splitn(2, char::is_whitespace);
            let tag = it.next()?.trim();
            let mut tb = Table::new();
            tb.set_str("tag", tag);
            if let Some(w) = it.next() {
                tb.set("window", sel(w.trim()));
            }
            t(dsp_table("window.tag", tb))
        }
        "signalwindow" => {
            // `signalwindow window,signal`
            let (w, s) = a.split_once(',')?;
            let mut tb = Table::new();
            tb.set("signal", Expr::infer(s.trim()))
                .set("window", sel(w.trim()));
            t(dsp_table("window.signal", tb))
        }
        "setprop" => {
            // `setprop window prop value [lock]`
            let mut it = a.split_whitespace();
            let window = it.next()?;
            let prop = it.next()?;
            let value = it.next()?;
            let mut tb = Table::new();
            tb.set_str("prop", prop)
                .set("value", Expr::infer(value))
                .set("window", sel(window));
            t(dsp_table("window.set_prop", tb))
        }
        "denywindowfromgroup" => t(dsp(
            "window.deny_from_group",
            &[Expr::str(if empty { "toggle" } else { a })],
        )),

        // ---- groups ----------------------------------------------------
        "togglegroup" => t(dsp("group.toggle", &[])),
        "changegroupactive" => {
            let mut tb = Table::new();
            tb.set("index", Expr::infer(if empty { "f" } else { a }));
            t(dsp_table("group.active", tb))
        }
        "lockgroups" => {
            let mut tb = Table::new();
            tb.set_str("mode", if empty { "toggle" } else { a });
            t(dsp_table("group.lock", tb))
        }
        "lockactivegroup" => {
            let mut tb = Table::new();
            tb.set_str("mode", if empty { "toggle" } else { a });
            t(dsp_table("group.lock_active", tb))
        }
        "movegroupwindow" => t(dsp(
            "group.move_window",
            &[Expr::str(if empty { "f" } else { a })],
        )),

        // ---- workspaces -------------------------------------------------
        "togglespecialworkspace" => {
            if empty {
                t(dsp("workspace.toggle_special", &[]))
            } else {
                t(dsp("workspace.toggle_special", &[Expr::str(a)]))
            }
        }
        "renameworkspace" => {
            let (id, name) = a.split_once(char::is_whitespace)?;
            let mut tb = Table::new();
            tb.set("workspace", sel(id.trim()))
                .set_str("name", name.trim());
            t(dsp_table("workspace.rename", tb))
        }
        "movecurrentworkspacetomonitor" => {
            let mut tb = Table::new();
            tb.set("monitor", sel(a));
            t(dsp_table("workspace.move", tb))
        }
        "moveworkspacetomonitor" => {
            let (ws, mon) = two(a)?;
            let mut tb = Table::new();
            tb.set("workspace", sel(ws)).set("monitor", sel(mon));
            t(dsp_table("workspace.move", tb))
        }
        "swapactiveworkspaces" => {
            let (m1, m2) = two(a)?;
            let mut tb = Table::new();
            tb.set("monitor1", sel(m1)).set("monitor2", sel(m2));
            t(dsp_table("workspace.swap_monitors", tb))
        }

        // ---- cursor -----------------------------------------------------
        "movecursortocorner" => {
            let mut tb = Table::new();
            tb.set("corner", Expr::infer(if empty { "0" } else { a }));
            t(dsp_table("cursor.move_to_corner", tb))
        }
        "movecursor" => {
            let (x, y) = two(a)?;
            let mut tb = Table::new();
            tb.set("x", Expr::infer(x)).set("y", Expr::infer(y));
            t(dsp_table("cursor.move", tb))
        }

        // ---- passthrough / shortcuts ------------------------------------
        "pass" => {
            let mut tb = Table::new();
            tb.set("window", sel(a));
            t(dsp_table("pass", tb))
        }
        "sendshortcut" => {
            // `sendshortcut MODS, KEY, window`
            let parts: Vec<&str> = a.splitn(3, ',').map(str::trim).collect();
            if parts.len() < 2 {
                return None;
            }
            let mut tb = Table::new();
            tb.set_str("mods", parts[0]).set_str("key", parts[1]);
            if let Some(w) = parts.get(2) {
                if !w.is_empty() {
                    tb.set("window", sel(w));
                }
            }
            t(dsp_table("send_shortcut", tb))
        }

        // Mouse-drag binds: `bindm = MOD, mouse:272, movewindow` means "start
        // an interactive drag", which is a different dispatcher in Lua than the
        // directional `movewindow` handled above. The caller disambiguates via
        // `to_lua_mouse`.
        _ => None,
    }
}

/// `resizeactive`/`moveactive` take `X Y`; the `*pixel` variants take
/// `X Y,window`. Returns `(x, y, window)`.
fn pixel_args(a: &str, has_window: bool) -> Option<(String, String, Option<String>)> {
    if has_window {
        let (coords, window) = a.rsplit_once(',')?;
        let (x, y) = two(coords.trim())?;
        Some((
            x.to_string(),
            y.to_string(),
            Some(window.trim().to_string()),
        ))
    } else {
        let (x, y) = two(a)?;
        Some((x.to_string(), y.to_string(), None))
    }
}

/// Translate a **mouse** bind's dispatcher (`bindm = ..., movewindow`).
///
/// `bindm` binds are interactive drags, so `movewindow`/`resizewindow` mean
/// "begin dragging/resizing under the cursor" rather than the directional
/// dispatchers of the same name.
#[must_use]
pub fn to_lua_mouse(dispatcher: &str, args: &str) -> Translated {
    match dispatcher.trim() {
        "movewindow" => Translated::native(dsp("window.drag", &[])),
        "resizewindow" => Translated::native(dsp("window.resize", &[])),
        other => to_lua(other, args),
    }
}

/// Recover a conf dispatcher + argument string from a Lua `hl.dsp.*` call.
///
/// Used when reading a Lua config back into the format-agnostic model so that
/// keybinds display (and convert back to `.conf`) sensibly. Returns `None` for
/// shapes we do not recognise; the caller then keeps the raw Lua text.
#[must_use]
pub fn from_lua(
    path: &str,
    fields: &[(String, String)],
    positional: &[String],
) -> Option<(String, String)> {
    let get = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    let first = || positional.first().map(String::as_str).unwrap_or("");
    let joined = || positional.join(" ");

    let pair = match path {
        "exec_cmd" => ("exec", first().to_string()),
        "exec_raw" => {
            // Undo the `hyprctl dispatch ...` shim so a Lua -> conf round-trip
            // recovers the original dispatcher rather than nesting shims.
            let cmd = first();
            if let Some(rest) = cmd.strip_prefix("hyprctl dispatch ") {
                let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                return Some((name.to_string(), args.to_string()));
            }
            ("execr", cmd.to_string())
        }
        "exit" => ("exit", String::new()),
        "force_renderer_reload" => ("forcerendererreload", String::new()),
        "force_idle" => ("forceidle", joined()),
        "global" => ("global", first().to_string()),
        "event" => ("event", first().to_string()),
        "submap" => ("submap", first().to_string()),
        "layout" => {
            // `hl.dsp.layout("togglesplit")` came from the `togglesplit`
            // dispatcher; anything else is a generic `layoutmsg`.
            let msg = first();
            match msg {
                "togglesplit" | "swapsplit" => (msg, String::new()),
                _ => ("layoutmsg", msg.to_string()),
            }
        }
        "dpms" => ("dpms", joined()),

        "window.close" => match get("window") {
            Some(w) => ("closewindow", w.to_string()),
            None => ("killactive", String::new()),
        },
        "window.kill" => match get("window") {
            Some(w) => ("killwindow", w.to_string()),
            None => ("forcekillactive", String::new()),
        },
        "window.float" => {
            let name = match get("action") {
                Some("set") => "setfloating",
                Some("unset") => "settiled",
                _ => "togglefloating",
            };
            (name, get("window").unwrap_or_default().to_string())
        }
        "window.pseudo" => ("pseudo", String::new()),
        "window.pin" => ("pin", get("window").unwrap_or_default().to_string()),
        "window.center" => ("centerwindow", joined()),
        "window.bring_to_top" => ("bringactivetotop", String::new()),
        "window.toggle_swallow" => ("toggleswallow", String::new()),
        "window.drag" => ("movewindow", String::new()),
        "window.cycle_next" => {
            let mut args = Vec::new();
            if get("previous").is_some_and(|v| v == "true") {
                args.push("prev");
            }
            for k in ["tiled", "floating", "visible"] {
                if get(k).is_some_and(|v| v == "true") {
                    args.push(k);
                }
            }
            ("cyclenext", args.join(" "))
        }
        "window.fullscreen" => match get("mode") {
            Some("maximized") => ("fullscreen", "1".to_string()),
            _ => ("fullscreen", String::new()),
        },
        "window.fullscreen_state" => {
            let internal = get("internal").unwrap_or("0");
            let client = get("client").unwrap_or("0");
            ("fullscreenstate", format!("{internal} {client}"))
        }
        "window.move" => {
            if let Some(dir) = get("direction") {
                ("movewindow", dir.to_string())
            } else if let Some(ws) = get("workspace") {
                let silent = get("silent").is_some_and(|v| v == "true");
                let name = if silent {
                    "movetoworkspacesilent"
                } else {
                    "movetoworkspace"
                };
                (name, ws.to_string())
            } else if let Some(mon) = get("monitor") {
                ("movewindow", format!("mon:{mon}"))
            } else {
                let x = get("x")?;
                let y = get("y")?;
                match get("window") {
                    Some(w) => ("movewindowpixel", format!("{x} {y},{w}")),
                    None => ("moveactive", format!("{x} {y}")),
                }
            }
        }
        "window.resize" => {
            let (Some(x), Some(y)) = (get("x"), get("y")) else {
                // `hl.dsp.window.resize()` with no args is the mouse drag form.
                return Some(("resizewindow".to_string(), String::new()));
            };
            match get("window") {
                Some(w) => ("resizewindowpixel", format!("{x} {y},{w}")),
                None => ("resizeactive", format!("{x} {y}")),
            }
        }
        "window.swap" => ("swapwindow", get("direction")?.to_string()),
        "window.alter_zorder" => {
            let mode = get("mode")?;
            match get("window") {
                Some(w) => ("alterzorder", format!("{mode},{w}")),
                None => ("alterzorder", mode.to_string()),
            }
        }
        "window.tag" => {
            let tag = get("tag")?;
            match get("window") {
                Some(w) => ("tagwindow", format!("{tag} {w}")),
                None => ("tagwindow", tag.to_string()),
            }
        }
        "window.signal" => {
            let signal = get("signal")?;
            let window = get("window").unwrap_or("activewindow");
            ("signalwindow", format!("{window},{signal}"))
        }
        "window.set_prop" => {
            let prop = get("prop")?;
            let value = get("value")?;
            let window = get("window").unwrap_or("activewindow");
            ("setprop", format!("{window} {prop} {value}"))
        }
        "window.deny_from_group" => ("denywindowfromgroup", first().to_string()),

        "focus" => {
            if let Some(dir) = get("direction") {
                ("movefocus", dir.to_string())
            } else if let Some(ws) = get("workspace") {
                ("workspace", ws.to_string())
            } else if let Some(w) = get("window") {
                ("focuswindow", w.to_string())
            } else if let Some(m) = get("monitor") {
                ("focusmonitor", m.to_string())
            } else if get("urgent_or_last").is_some() {
                ("focusurgentorlast", String::new())
            } else if get("current_or_last").is_some() {
                ("focuscurrentorlast", String::new())
            } else {
                return None;
            }
        }

        "group.toggle" => ("togglegroup", String::new()),
        "group.active" => ("changegroupactive", get("index").unwrap_or("f").to_string()),
        "group.lock" => ("lockgroups", get("mode").unwrap_or("toggle").to_string()),
        "group.lock_active" => (
            "lockactivegroup",
            get("mode").unwrap_or("toggle").to_string(),
        ),
        "group.move_window" => ("movegroupwindow", first().to_string()),

        "workspace.toggle_special" => ("togglespecialworkspace", first().to_string()),
        "workspace.rename" => {
            let ws = get("workspace")?;
            let name = get("name")?;
            ("renameworkspace", format!("{ws} {name}"))
        }
        "workspace.move" => match (get("workspace"), get("monitor")) {
            (Some(ws), Some(m)) => ("moveworkspacetomonitor", format!("{ws} {m}")),
            (None, Some(m)) => ("movecurrentworkspacetomonitor", m.to_string()),
            _ => return None,
        },
        "workspace.swap_monitors" => {
            let m1 = get("monitor1")?;
            let m2 = get("monitor2")?;
            ("swapactiveworkspaces", format!("{m1} {m2}"))
        }

        "cursor.move" => {
            let x = get("x")?;
            let y = get("y")?;
            ("movecursor", format!("{x} {y}"))
        }
        "cursor.move_to_corner" => ("movecursortocorner", get("corner")?.to_string()),

        "pass" => ("pass", get("window").unwrap_or_default().to_string()),
        "send_shortcut" => {
            let mods = get("mods")?;
            let key = get("key")?;
            match get("window") {
                Some(w) => ("sendshortcut", format!("{mods}, {key}, {w}")),
                None => ("sendshortcut", format!("{mods}, {key},")),
            }
        }

        _ => return None,
    };

    Some((pair.0.to_string(), pair.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lua(d: &str, a: &str) -> String {
        to_lua(d, a).lua
    }

    #[test]
    fn simple_dispatchers_have_native_forms() {
        assert_eq!(lua("killactive", ""), "hl.dsp.window.close()");
        assert_eq!(lua("exit", ""), "hl.dsp.exit()");
        assert_eq!(lua("pseudo", ""), "hl.dsp.window.pseudo()");
        assert_eq!(lua("exec", "kitty"), "hl.dsp.exec_cmd(\"kitty\")");
        assert_eq!(lua("togglesplit", ""), "hl.dsp.layout(\"togglesplit\")");
    }

    #[test]
    fn directions_expand_from_shorthand() {
        assert_eq!(
            lua("movefocus", "l"),
            "hl.dsp.focus({ direction = \"left\" })"
        );
        assert_eq!(
            lua("movewindow", "d"),
            "hl.dsp.window.move({ direction = \"down\" })"
        );
        assert_eq!(
            lua("swapwindow", "up"),
            "hl.dsp.window.swap({ direction = \"up\" })"
        );
    }

    #[test]
    fn workspace_targets_keep_their_natural_type() {
        assert_eq!(lua("workspace", "3"), "hl.dsp.focus({ workspace = 3 })");
        assert_eq!(
            lua("workspace", "e+1"),
            "hl.dsp.focus({ workspace = \"e+1\" })"
        );
        assert_eq!(
            lua("movetoworkspacesilent", "special:magic"),
            "hl.dsp.window.move({ workspace = \"special:magic\", silent = true })"
        );
    }

    #[test]
    fn fullscreen_modes_map_to_the_lua_vocabulary() {
        assert_eq!(lua("fullscreen", ""), "hl.dsp.window.fullscreen()");
        assert_eq!(
            lua("fullscreen", "1"),
            "hl.dsp.window.fullscreen({ mode = \"maximized\" })"
        );
        // Mode 2 has no single-call equivalent and is flagged for review.
        let t = to_lua("fullscreen", "2");
        assert_eq!(t.fidelity, Fidelity::Approximate);
        assert!(t.lua.starts_with("hl.dsp.window.fullscreen_state("));
    }

    #[test]
    fn pixel_dispatchers_split_coordinates_and_selectors() {
        assert_eq!(
            lua("resizeactive", "10 0"),
            "hl.dsp.window.resize({ x = 10, y = 0 })"
        );
        assert_eq!(
            lua("movewindowpixel", "10 20,class:^(kitty)$"),
            "hl.dsp.window.move({ x = 10, y = 20, window = \"class:^(kitty)$\" })"
        );
    }

    #[test]
    fn mouse_binds_become_interactive_drags() {
        assert_eq!(to_lua_mouse("movewindow", "").lua, "hl.dsp.window.drag()");
        assert_eq!(
            to_lua_mouse("resizewindow", "").lua,
            "hl.dsp.window.resize()"
        );
    }

    #[test]
    fn unknown_dispatchers_fall_back_to_a_working_shim() {
        let t = to_lua("someplugin:dowhatever", "arg1 arg2");
        assert_eq!(t.fidelity, Fidelity::Shim);
        assert_eq!(
            t.lua,
            "hl.dsp.exec_raw(\"hyprctl dispatch someplugin:dowhatever arg1 arg2\")"
        );
        assert!(t.note.is_some());
    }

    fn back(path: &str, fields: &[(&str, &str)], pos: &[&str]) -> Option<(String, String)> {
        let f: Vec<(String, String)> = fields
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let p: Vec<String> = pos.iter().map(|s| (*s).to_string()).collect();
        from_lua(path, &f, &p)
    }

    #[test]
    fn round_trips_through_from_lua() {
        for (d, a) in [
            ("killactive", ""),
            ("exec", "kitty"),
            ("movefocus", "left"),
            ("workspace", "3"),
            ("movetoworkspacesilent", "special:magic"),
            ("resizeactive", "10 0"),
            ("togglespecialworkspace", "magic"),
            ("swapwindow", "left"),
            ("togglegroup", ""),
            ("renameworkspace", "1 web"),
        ] {
            let t = to_lua(d, a);
            assert_eq!(t.fidelity, Fidelity::Native, "{d} should be native");
            // Re-derive the conf form from the structural pieces the mapper
            // would extract, and check it matches what we started from.
            let (path, fields, pos) = parse_call(&t.lua);
            let got = back(
                &path,
                &fields
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect::<Vec<_>>(),
                &pos.iter().map(String::as_str).collect::<Vec<_>>(),
            )
            .unwrap_or_else(|| panic!("no reverse mapping for {d}"));
            assert_eq!(got, (d.to_string(), a.to_string()), "round-trip for {d}");
        }
    }

    #[test]
    fn shim_round_trips_back_to_the_original_dispatcher() {
        let t = to_lua("myplugin:thing", "a b");
        let (path, fields, pos) = parse_call(&t.lua);
        let got = back(
            &path,
            &fields
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect::<Vec<_>>(),
            &pos.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert_eq!(got, Some(("myplugin:thing".into(), "a b".into())));
    }

    /// Minimal parser for the `hl.dsp.<path>(...)` text this module emits, used
    /// only to exercise the reverse mapping in tests.
    fn parse_call(src: &str) -> (String, Vec<(String, String)>, Vec<String>) {
        let rest = src.strip_prefix("hl.dsp.").expect("dsp call");
        let (path, args) = rest.split_once('(').expect("open paren");
        let args = args.strip_suffix(')').expect("close paren").trim();
        let mut fields = Vec::new();
        let mut positional = Vec::new();
        if let Some(inner) = args.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            for part in split_top(inner) {
                if let Some((k, v)) = part.split_once('=') {
                    fields.push((k.trim().to_string(), unquote(v.trim())));
                }
            }
        } else if !args.is_empty() {
            for part in split_top(args) {
                positional.push(unquote(part.trim()));
            }
        }
        (path.to_string(), fields, positional)
    }

    fn split_top(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut depth = 0usize;
        let mut in_str = false;
        let mut cur = String::new();
        let mut prev = '\0';
        for c in s.chars() {
            match c {
                '"' if prev != '\\' => in_str = !in_str,
                '{' if !in_str => depth += 1,
                '}' if !in_str => depth -= 1,
                ',' if !in_str && depth == 0 => {
                    out.push(std::mem::take(&mut cur));
                    prev = c;
                    continue;
                }
                _ => {}
            }
            cur.push(c);
            prev = c;
        }
        if !cur.trim().is_empty() {
            out.push(cur);
        }
        out
    }

    fn unquote(s: &str) -> String {
        s.strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .map_or_else(|| s.to_string(), |inner| inner.replace("\\\"", "\""))
    }
}

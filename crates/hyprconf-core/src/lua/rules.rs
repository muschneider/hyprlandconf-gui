// SPDX-License-Identifier: MIT OR Apache-2.0
//! Translation of window / layer / workspace **rules** between the hyprlang
//! string form and the Lua table form.
//!
//! # Why this exists
//!
//! `.conf` encodes a rule as free text plus a matcher string:
//!
//! ```text
//! windowrulev2 = opacity 0.9 0.9, class:^(kitty)$
//! workspace    = 2, monitor:DP-1, default:true
//! ```
//!
//! Lua encodes the same thing as a typed table where the rule is a *field*:
//!
//! ```lua
//! hl.window_rule({ name = "...", match = { class = "^(kitty)$" }, opacity = "0.9 0.9" })
//! hl.workspace_rule({ workspace = "2", monitor = "DP-1", default = true })
//! ```
//!
//! Hyprland rejects unknown fields (`hl.window_rule: unknown field 'no_border'`)
//! but — critically — it does **not** reject a rule that simply has no effect.
//! Putting the rule text in `name` (which is only a handle used by
//! `set_enabled`) loads without error and silently does nothing, so getting this
//! mapping right matters more than it might appear.
//!
//! The accepted field names below were enumerated by probing Hyprland 0.56.1
//! with `Hyprland --verify-config`.

use crate::lua::emit::{Expr, Table};

/// A rule that could not be expressed as a Lua field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmappedRule {
    /// The original hyprlang rule text.
    pub text: String,
    /// Why it could not be mapped.
    pub reason: String,
}

/// Window-rule names that take no argument (`float`, `pin`, ...), mapped from
/// their hyprlang spelling to the Lua field name.
const WINDOW_FLAGS: &[(&str, &str)] = &[
    ("float", "float"),
    ("tile", "tile"),
    ("fullscreen", "fullscreen"),
    ("maximize", "maximize"),
    ("pin", "pin"),
    ("center", "center"),
    ("noanim", "no_anim"),
    ("nofocus", "no_focus"),
    ("noinitialfocus", "no_initial_focus"),
    ("noblur", "no_blur"),
    ("noshadow", "no_shadow"),
    ("nodim", "no_dim"),
    ("nomaxsize", "no_max_size"),
    ("nofollowmouse", "no_follow_mouse"),
    ("noscreenshare", "no_screen_share"),
    ("opaque", "opaque"),
    ("immediate", "immediate"),
    ("stayfocused", "stay_focused"),
    ("keepaspectratio", "keep_aspect_ratio"),
    ("persistentsize", "persistent_size"),
    ("dimaround", "dim_around"),
    ("xray", "xray"),
    ("nearestneighbor", "nearest_neighbor"),
    ("renderunfocused", "render_unfocused"),
    ("syncfullscreen", "sync_fullscreen"),
    ("decorate", "decorate"),
    ("focusonactivate", "focus_on_activate"),
    ("allowsinput", "allows_input"),
    ("scrollmouse", "scroll_mouse"),
    ("scrolltouchpad", "scroll_touchpad"),
];

/// Window-rule names that take an argument, mapped to their Lua field name.
/// The boolean marks fields whose value is numeric rather than free text.
const WINDOW_ARGS: &[(&str, &str, bool)] = &[
    ("opacity", "opacity", false),
    ("size", "size", false),
    ("minsize", "min_size", false),
    ("maxsize", "max_size", false),
    ("move", "move", false),
    ("workspace", "workspace", false),
    ("monitor", "monitor", false),
    ("animation", "animation", false),
    ("bordercolor", "border_color", false),
    ("bordersize", "border_size", true),
    ("rounding", "rounding", true),
    ("suppressevent", "suppress_event", false),
    ("tag", "tag", false),
    ("group", "group", false),
    ("idleinhibit", "idle_inhibit", false),
    ("content", "content", false),
    ("xray", "xray", false),
    ("scrollmouse", "scroll_mouse", false),
    ("scrolltouchpad", "scroll_touchpad", false),
    ("plugin", "plugin", false),
];

/// Layer-rule names that take no argument.
const LAYER_FLAGS: &[(&str, &str)] = &[
    ("noanim", "no_anim"),
    ("blur", "blur"),
    ("blurpopups", "blur_popups"),
    ("dimaround", "dim_around"),
    ("xray", "xray"),
    ("noscreenshare", "no_screen_share"),
    ("abovelock", "above_lock"),
];

/// Layer-rule names that take an argument.
const LAYER_ARGS: &[(&str, &str)] = &[
    ("ignorealpha", "ignore_alpha"),
    ("ignorezero", "ignore_alpha"),
    ("animation", "animation"),
    ("order", "order"),
    ("abovelock", "above_lock"),
    ("xray", "xray"),
];

/// Workspace-rule keys (`key:value`), mapped to their Lua field name.
const WORKSPACE_KEYS: &[(&str, &str)] = &[
    ("monitor", "monitor"),
    ("default", "default"),
    ("gapsin", "gaps_in"),
    ("gapsout", "gaps_out"),
    ("floatgaps", "float_gaps"),
    ("bordersize", "border_size"),
    ("border", "no_border"),
    ("shadow", "no_shadow"),
    ("rounding", "no_rounding"),
    ("decorate", "decorate"),
    ("persistent", "persistent"),
    ("defaultname", "default_name"),
    ("on-created-empty", "on_created_empty"),
    ("oncreatedempty", "on_created_empty"),
    ("layoutopt", "layout_opts"),
    ("layout", "layout"),
    ("animation", "animation"),
];

/// Parse a hyprlang **window** rule body into Lua table fields.
///
/// Returns the fields to merge into the `hl.window_rule` table, or an
/// [`UnmappedRule`] when the rule name is not recognised.
pub fn window_rule_fields(rule: &str) -> Result<Vec<(String, Expr)>, UnmappedRule> {
    let text = rule.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }

    let (head, rest) = split_head(text);
    let head_lc = head.to_ascii_lowercase();

    // `nomaxsize` etc. are flags; check those first so `size`-prefixed names
    // are not mistaken for the argument form.
    if rest.is_empty() {
        if let Some((_, field)) = WINDOW_FLAGS.iter().find(|(n, _)| *n == head_lc) {
            return Ok(vec![((*field).to_string(), Expr::Bool(true))]);
        }
    }

    if let Some((_, field, numeric)) = WINDOW_ARGS.iter().find(|(n, _, _)| *n == head_lc) {
        if rest.is_empty() {
            return Err(UnmappedRule {
                text: text.to_string(),
                reason: format!("`{head}` expects a value"),
            });
        }
        let value = if *numeric {
            Expr::infer(rest)
        } else {
            Expr::str(rest)
        };
        return Ok(vec![((*field).to_string(), value)]);
    }

    // Some flags also accept an optional argument (`xray on`).
    if let Some((_, field)) = WINDOW_FLAGS.iter().find(|(n, _)| *n == head_lc) {
        return Ok(vec![((*field).to_string(), bool_or_str(rest))]);
    }

    Err(UnmappedRule {
        text: text.to_string(),
        reason: format!("`{head}` is not a window rule the Lua API exposes"),
    })
}

/// Normalise a rule *value* that came from the `.conf` block form, where fields
/// already use the Lua spelling but values still use hyprlang's vocabulary.
#[must_use]
pub fn normalise_block_value(field: &str, value: &str) -> String {
    // Only flag-style fields are boolean; `size = 1100 800` must stay text.
    let is_flag = WINDOW_FLAGS.iter().any(|(_, f)| *f == field)
        || LAYER_FLAGS.iter().any(|(_, f)| *f == field);
    if !is_flag {
        return value.to_string();
    }
    match hypr_bool(value) {
        Some(b) => b.to_string(),
        None => value.to_string(),
    }
}

/// Parse a hyprlang **layer** rule body into Lua table fields.
pub fn layer_rule_fields(rule: &str) -> Result<Vec<(String, Expr)>, UnmappedRule> {
    let text = rule.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let (head, rest) = split_head(text);
    let head_lc = head.to_ascii_lowercase();

    if rest.is_empty() {
        if let Some((_, field)) = LAYER_FLAGS.iter().find(|(n, _)| *n == head_lc) {
            return Ok(vec![((*field).to_string(), Expr::Bool(true))]);
        }
    }
    if let Some((name, field)) = LAYER_ARGS.iter().find(|(n, _)| *n == head_lc) {
        // `ignorezero` is `ignorealpha 0`.
        if *name == "ignorezero" {
            return Ok(vec![((*field).to_string(), Expr::Int(0))]);
        }
        if rest.is_empty() {
            return Err(UnmappedRule {
                text: text.to_string(),
                reason: format!("`{head}` expects a value"),
            });
        }
        return Ok(vec![((*field).to_string(), Expr::infer(rest))]);
    }
    if let Some((_, field)) = LAYER_FLAGS.iter().find(|(n, _)| *n == head_lc) {
        return Ok(vec![((*field).to_string(), bool_or_str(rest))]);
    }

    Err(UnmappedRule {
        text: text.to_string(),
        reason: format!("`{head}` is not a layer rule the Lua API exposes"),
    })
}

/// Every `.conf` window-rule effect this converter understands, as
/// `(name, takes_an_argument)` — flags first. The GUI offers exactly these, so
/// a rule picked there is guaranteed to survive a `.conf` → Lua conversion.
#[must_use]
pub fn window_rule_effects() -> Vec<(&'static str, bool)> {
    let mut out: Vec<(&'static str, bool)> =
        WINDOW_FLAGS.iter().map(|(c, _)| (*c, false)).collect();
    for (conf, _, _) in WINDOW_ARGS {
        if !out.iter().any(|(n, _)| n == conf) {
            out.push((conf, true));
        }
    }
    out
}

/// Every `.conf` layer-rule effect this converter understands, as
/// `(name, takes_an_argument)`.
#[must_use]
pub fn layer_rule_effects() -> Vec<(&'static str, bool)> {
    let mut out: Vec<(&'static str, bool)> = LAYER_FLAGS.iter().map(|(c, _)| (*c, false)).collect();
    for (conf, _) in LAYER_ARGS {
        if !out.iter().any(|(n, _)| n == conf) {
            out.push((conf, true));
        }
    }
    out
}

/// The `.conf` workspace-rule keys (`monitor`, `gapsin`, `persistent`, …).
#[must_use]
pub fn workspace_rule_keys() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for (conf, _) in WORKSPACE_KEYS {
        // `oncreatedempty` is an alternative spelling of `on-created-empty`.
        if *conf != "oncreatedempty" {
            out.push(conf);
        }
    }
    out
}

/// Parse a hyprlang **workspace** rule list (`monitor:DP-1, default:true`) into
/// Lua table fields.
pub fn workspace_rule_fields(rules: &str) -> (Vec<(String, Expr)>, Vec<UnmappedRule>) {
    let mut fields = Vec::new();
    let mut unmapped = Vec::new();

    for part in split_rules(rules) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (key, value) = match part.split_once(':') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (part, "true"),
        };
        let key_lc = key.to_ascii_lowercase();
        match WORKSPACE_KEYS.iter().find(|(n, _)| *n == key_lc) {
            Some((name, field)) => {
                // `border:false` -> `no_border = true` (and vice versa).
                let inverted = matches!(*name, "border" | "shadow" | "rounding");
                let expr = if inverted {
                    Expr::Bool(!matches!(value, "true" | "1" | "yes" | "on"))
                } else {
                    Expr::infer(value)
                };
                fields.push(((*field).to_string(), expr));
            }
            None => unmapped.push(UnmappedRule {
                text: part.to_string(),
                reason: format!("`{key}` is not a workspace rule the Lua API exposes"),
            }),
        }
    }

    (fields, unmapped)
}

/// Parse a hyprlang **matcher** string into a Lua `match` table.
///
/// Handles both v2 `key:value, key:value` matchers and the legacy v1 form,
/// where the whole string is a bare class regex.
#[must_use]
pub fn matcher_table(matchers: &str, v2: bool) -> Table {
    let mut table = Table::new();
    let text = matchers.trim();
    if text.is_empty() {
        return table;
    }

    if !v2 {
        // Legacy `windowrule = float, ^(kitty)$` matches on class only.
        table.set_str("class", text);
        return table;
    }

    for part in split_rules(text) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once(':') {
            Some((k, v)) => {
                let key = matcher_key(k.trim());
                table.set(key, bool_or_str(v.trim()));
            }
            // A bare token in a v2 matcher is a class regex.
            None => {
                table.set_str("class", part);
            }
        }
    }
    table
}

/// Normalise a v2 matcher key to its Lua spelling.
fn matcher_key(key: &str) -> String {
    match key.to_ascii_lowercase().as_str() {
        "initialclass" => "initial_class".to_string(),
        "initialtitle" => "initial_title".to_string(),
        "onworkspace" => "workspace".to_string(),
        "floating" => "float".to_string(),
        "fullscreenstate" => "fullscreen_state".to_string(),
        "contenttype" => "content_type".to_string(),
        "xdgtag" => "xdg_tag".to_string(),
        other => other.to_string(),
    }
}

/// Split on commas that are not inside a regex group or bracket expression.
///
/// Matchers routinely contain commas inside `(a|b){1,2}` style regexes, so a
/// naive `split(',')` corrupts them.
fn split_rules(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut escaped = false;
    for c in s.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' => {
                cur.push(c);
                escaped = true;
            }
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth <= 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Split `"opacity 0.9 0.9"` into `("opacity", "0.9 0.9")`.
fn split_head(text: &str) -> (&str, &str) {
    match text.split_once(char::is_whitespace) {
        Some((h, r)) => (h, r.trim()),
        None => (text, ""),
    }
}

/// Interpret a hyprlang boolean, or `None` if the text is not one.
///
/// `.conf` accepts `yes`/`on`/`1` where Lua insists on a real `true`, so a rule
/// written `float = yes` must not be carried across as the *string* `"yes"` —
/// Hyprland rejects it with *"field 'float': boolean type requires a bool"*.
fn hypr_bool(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// A hyprlang boolean becomes a Lua boolean; everything else stays a string.
fn bool_or_str(text: &str) -> Expr {
    match hypr_bool(text) {
        Some(b) => Expr::Bool(b),
        None => Expr::str(text),
    }
}

/// Render a set of Lua rule fields back into a hyprlang window-rule body.
///
/// Used when converting Lua -> conf. Returns `None` for fields that are part of
/// the table but not rules (`name`, `match`, `enabled`).
#[must_use]
pub fn window_field_to_conf(field: &str, value: &str) -> Option<String> {
    if matches!(field, "name" | "match" | "enabled") {
        return None;
    }
    if let Some((name, _)) = WINDOW_FLAGS.iter().find(|(_, f)| *f == field) {
        return Some(if value == "true" {
            (*name).to_string()
        } else {
            format!("{name} {value}")
        });
    }
    if let Some((name, _, _)) = WINDOW_ARGS.iter().find(|(_, f, _)| *f == field) {
        return Some(format!("{name} {value}"));
    }
    None
}

/// Render a Lua layer-rule field back into a hyprlang layer-rule body.
#[must_use]
pub fn layer_field_to_conf(field: &str, value: &str) -> Option<String> {
    if matches!(field, "name" | "match" | "enabled") {
        return None;
    }
    if let Some((name, _)) = LAYER_FLAGS.iter().find(|(_, f)| *f == field) {
        return Some(if value == "true" {
            (*name).to_string()
        } else {
            format!("{name} {value}")
        });
    }
    if let Some((name, _)) = LAYER_ARGS.iter().find(|(_, f)| *f == field) {
        return Some(format!("{name} {value}"));
    }
    None
}

/// Render a Lua workspace-rule field back into a hyprlang `key:value` pair.
#[must_use]
pub fn workspace_field_to_conf(field: &str, value: &str) -> Option<String> {
    if matches!(field, "workspace" | "enabled") {
        return None;
    }
    let (name, _) = WORKSPACE_KEYS.iter().find(|(_, f)| *f == field)?;
    let inverted = matches!(*name, "border" | "shadow" | "rounding");
    let value = if inverted {
        if value == "true" {
            "false"
        } else {
            "true"
        }
    } else {
        value
    };
    Some(format!("{name}:{value}"))
}

/// Convert a Lua `match` table back into a hyprlang v2 matcher string.
#[must_use]
pub fn matchers_to_conf(fields: &[(String, String)]) -> String {
    fields
        .iter()
        .map(|(k, v)| {
            let key = match k.as_str() {
                "initial_class" => "initialclass",
                "initial_title" => "initialtitle",
                "float" => "floating",
                "fullscreen_state" => "fullscreenstate",
                "content_type" => "contenttype",
                "xdg_tag" => "xdgtag",
                other => other,
            };
            format!("{key}:{v}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(rule: &str) -> Vec<(String, String)> {
        window_rule_fields(rule)
            .unwrap()
            .into_iter()
            .map(|(k, v)| (k, v.render()))
            .collect()
    }

    #[test]
    fn window_flags_become_boolean_fields() {
        assert_eq!(fields("float"), vec![("float".into(), "true".into())]);
        assert_eq!(fields("noanim"), vec![("no_anim".into(), "true".into())]);
        assert_eq!(
            fields("noinitialfocus"),
            vec![("no_initial_focus".into(), "true".into())]
        );
    }

    #[test]
    fn window_arguments_keep_their_payload() {
        assert_eq!(
            fields("opacity 0.9 0.9"),
            vec![("opacity".into(), "\"0.9 0.9\"".into())]
        );
        // Numeric rules emit bare numbers so Hyprland's integer parser accepts them.
        assert_eq!(fields("rounding 0"), vec![("rounding".into(), "0".into())]);
        assert_eq!(
            fields("bordersize 2"),
            vec![("border_size".into(), "2".into())]
        );
        assert_eq!(
            fields("suppressevent maximize"),
            vec![("suppress_event".into(), "\"maximize\"".into())]
        );
    }

    #[test]
    fn unknown_window_rules_are_reported_not_silently_dropped() {
        let err = window_rule_fields("totallybogus 1").unwrap_err();
        assert!(err.reason.contains("totallybogus"));
    }

    #[test]
    fn v2_matchers_split_into_a_table() {
        let t = matcher_table("class:^(kitty)$, title:^(foo)$", true);
        assert_eq!(
            t.render_inline(),
            "{ class = \"^(kitty)$\", title = \"^(foo)$\" }"
        );
    }

    #[test]
    fn matcher_commas_inside_regex_groups_are_not_split() {
        let t = matcher_table("class:^(a|b){1,2}$, floating:true", true);
        assert_eq!(
            t.render_inline(),
            "{ class = \"^(a|b){1,2}$\", float = true }"
        );
    }

    #[test]
    fn legacy_v1_matchers_are_class_regexes() {
        let t = matcher_table("^(pavucontrol)$", false);
        assert_eq!(t.render_inline(), "{ class = \"^(pavucontrol)$\" }");
    }

    #[test]
    fn workspace_rules_map_and_invert_negatives() {
        let (f, unmapped) = workspace_rule_fields("monitor:DP-1, default:true, gapsout:0");
        assert!(unmapped.is_empty());
        let rendered: Vec<(String, String)> = f.into_iter().map(|(k, v)| (k, v.render())).collect();
        assert_eq!(
            rendered,
            vec![
                ("monitor".to_string(), "\"DP-1\"".to_string()),
                ("default".to_string(), "true".to_string()),
                ("gaps_out".to_string(), "0".to_string()),
            ]
        );

        // `border:false` is expressed as `no_border = true` in Lua.
        let (f, _) = workspace_rule_fields("border:false");
        assert_eq!(f[0].0, "no_border");
        assert_eq!(f[0].1.render(), "true");
    }

    #[test]
    fn layer_rules_map_flags_and_ignorezero() {
        let f = layer_rule_fields("noanim").unwrap();
        assert_eq!(f[0].0, "no_anim");
        let f = layer_rule_fields("ignorezero").unwrap();
        assert_eq!(f[0].0, "ignore_alpha");
        assert_eq!(f[0].1.render(), "0");
    }

    #[test]
    fn conf_round_trip_for_rule_fields() {
        assert_eq!(
            window_field_to_conf("float", "true").as_deref(),
            Some("float")
        );
        assert_eq!(
            window_field_to_conf("opacity", "0.9 0.9").as_deref(),
            Some("opacity 0.9 0.9")
        );
        assert_eq!(window_field_to_conf("name", "x"), None);
        assert_eq!(
            workspace_field_to_conf("gaps_out", "0").as_deref(),
            Some("gapsout:0")
        );
        assert_eq!(
            workspace_field_to_conf("no_border", "true").as_deref(),
            Some("border:false")
        );
    }

    #[test]
    fn matchers_convert_back_to_conf_spelling() {
        let f = vec![
            ("class".to_string(), "^(kitty)$".to_string()),
            ("float".to_string(), "true".to_string()),
        ];
        assert_eq!(matchers_to_conf(&f), "class:^(kitty)$, floating:true");
    }
}

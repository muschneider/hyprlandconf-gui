// SPDX-License-Identifier: MIT OR Apache-2.0
//! Serialization of a [`ConfDocument`] back to hyprlang text, and rendering of
//! typed [`Value`]s into their `.conf` textual form.
//!
//! The heavy lifting of round-trip fidelity lives in [`ConfDocument::to_text`];
//! this module is a thin, explicitly-named facade plus the value renderer used
//! by edits.

use std::fmt::Write as _;

use crate::conf::document::ConfDocument;
use crate::model::Config;
use crate::structured::{ExecKind, Keybind};
use crate::value::Value;

/// Emits valid hyprlang from a [`ConfDocument`].
#[derive(Debug, Default, Clone, Copy)]
pub struct ConfSerializer;

impl ConfSerializer {
    /// Serialize a document to text (byte-for-byte identical to the input when
    /// the document has not been edited).
    #[must_use]
    pub fn serialize(document: &ConfDocument) -> String {
        document.to_text()
    }

    /// Generate fresh `.conf` text from an in-memory [`Config`].
    #[must_use]
    pub fn serialize_config(config: &Config) -> String {
        config_to_conf(config)
    }
}

/// Generate fresh, valid hyprlang `.conf` text from an in-memory [`Config`].
///
/// Unlike [`ConfSerializer::serialize`] this does not preserve any original
/// formatting (there is none); it is used to write a config the GUI built from
/// scratch or to convert from the Lua format.
#[must_use]
pub fn config_to_conf(config: &Config) -> String {
    let mut out = String::new();

    for v in &config.variables {
        let _ = writeln!(out, "${} = {}", v.value.name, v.value.value);
    }
    blank(&mut out, !config.variables.is_empty());

    for (path, tracked) in &config.options {
        let _ = writeln!(out, "{path} = {}", value_to_conf(&tracked.value));
    }
    blank(&mut out, !config.options.is_empty());

    for e in &config.env {
        let _ = writeln!(out, "env = {}, {}", e.value.name, e.value.value);
    }
    for m in &config.monitors {
        let m = &m.value;
        let mut fields = vec![
            m.name.clone(),
            m.mode.clone(),
            m.position.clone(),
            m.scale.clone(),
        ];
        fields.extend(m.extra.iter().cloned());
        let _ = writeln!(out, "monitor = {}", fields.join(", "));
    }
    for w in &config.workspaces {
        let _ = writeln!(out, "workspace = {}, {}", w.value.selector, w.value.rules);
    }
    for r in &config.window_rules {
        let kw = if r.value.v2 {
            "windowrulev2"
        } else {
            "windowrule"
        };
        let _ = writeln!(out, "{kw} = {}, {}", r.value.rule, r.value.matchers);
    }
    for r in &config.layer_rules {
        let _ = writeln!(out, "layerrule = {}, {}", r.value.rule, r.value.namespace);
    }
    for b in &config.beziers {
        let b = &b.value;
        let _ = writeln!(
            out,
            "bezier = {}, {}, {}, {}, {}",
            b.name,
            num(b.p0.x),
            num(b.p0.y),
            num(b.p1.x),
            num(b.p1.y)
        );
    }
    for a in &config.animations {
        let a = &a.value;
        let onoff = if a.enabled { "1" } else { "0" };
        let mut line = format!(
            "animation = {}, {onoff}, {}, {}",
            a.name,
            num(a.speed),
            a.curve
        );
        if let Some(style) = &a.style {
            let _ = write!(line, ", {style}");
        }
        let _ = writeln!(out, "{line}");
    }
    for e in &config.execs {
        let kw = match e.value.kind {
            ExecKind::Exec => "exec",
            ExecKind::ExecOnce => "exec-once",
            ExecKind::ExecShutdown => "exec-shutdown",
        };
        let _ = writeln!(out, "{kw} = {}", e.value.command);
    }

    write_keybinds(&mut out, config);

    out
}

/// Emit keybinds, grouping submap-scoped binds into `submap = NAME ... submap = reset`.
fn write_keybinds(out: &mut String, config: &Config) {
    let binds: Vec<&Keybind> = config.keybinds.iter().map(|t| &t.value).collect();

    for kb in binds.iter().filter(|k| k.submap.is_none()) {
        let _ = writeln!(out, "{}", keybind_line(kb));
    }

    // Distinct submaps in first-seen order.
    let mut submaps: Vec<&str> = Vec::new();
    for kb in &binds {
        if let Some(s) = &kb.submap {
            if !submaps.contains(&s.as_str()) {
                submaps.push(s);
            }
        }
    }
    for name in submaps {
        let _ = writeln!(out, "\nsubmap = {name}");
        for kb in binds.iter().filter(|k| k.submap.as_deref() == Some(name)) {
            let _ = writeln!(out, "{}", keybind_line(kb));
        }
        let _ = writeln!(out, "submap = reset");
    }
}

fn keybind_line(kb: &Keybind) -> String {
    let mut line = format!(
        "{} = {}, {}, {}",
        kb.flags.keyword(),
        kb.mods,
        kb.key,
        kb.dispatcher
    );
    if !kb.args.is_empty() {
        let _ = write!(line, ", {}", kb.args);
    }
    line
}

fn blank(out: &mut String, condition: bool) {
    if condition {
        out.push('\n');
    }
}

fn num(value: f64) -> String {
    format_f64(value)
}

/// Render a typed [`Value`] into its hyprlang `.conf` textual form.
#[must_use]
pub fn value_to_conf(value: &Value) -> String {
    match value {
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(x) => format_f64(*x),
        Value::Color(c) => c.to_rgba_string(),
        Value::Gradient(g) => g.to_hyprland_string(),
        Value::String(s) => s.clone(),
        Value::Enum(name) => name.clone(),
        Value::Vec2(v) => v.to_hyprland_string(),
    }
}

/// Format an `f64` without a trailing `.0` for whole numbers.
fn format_f64(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structured::{Exec, Keybind, KeybindFlags, MonitorRule, WindowRule};
    use crate::value::{Color, Vec2};
    use crate::Tracked;

    #[test]
    fn config_to_conf_emits_directives() {
        let mut config = Config::empty();
        config
            .options
            .insert("decoration:rounding".into(), Tracked::new(Value::Int(10)));
        config.keybinds.push(Tracked::new(Keybind {
            flags: KeybindFlags::default(),
            mods: "SUPER".into(),
            key: "Q".into(),
            dispatcher: "killactive".into(),
            args: String::new(),
            submap: None,
        }));
        config.window_rules.push(Tracked::new(WindowRule {
            v2: true,
            rule: "float".into(),
            matchers: "class:^(kitty)$".into(),
        }));
        config.monitors.push(Tracked::new(MonitorRule {
            name: "DP-1".into(),
            mode: "1920x1080@144".into(),
            position: "0x0".into(),
            scale: "1".into(),
            extra: vec!["transform".into(), "1".into()],
        }));
        config.execs.push(Tracked::new(Exec {
            kind: crate::structured::ExecKind::ExecOnce,
            command: "waybar".into(),
        }));

        let text = config_to_conf(&config);
        assert!(text.contains("decoration:rounding = 10"), "{text}");
        assert!(text.contains("bind = SUPER, Q, killactive"), "{text}");
        assert!(
            text.contains("windowrulev2 = float, class:^(kitty)$"),
            "{text}"
        );
        assert!(
            text.contains("monitor = DP-1, 1920x1080@144, 0x0, 1, transform, 1"),
            "{text}"
        );
        assert!(text.contains("exec-once = waybar"), "{text}");
    }

    #[test]
    fn renders_scalars() {
        assert_eq!(value_to_conf(&Value::Bool(true)), "true");
        assert_eq!(value_to_conf(&Value::Int(10)), "10");
        assert_eq!(value_to_conf(&Value::Float(0.5)), "0.5");
        assert_eq!(value_to_conf(&Value::Float(1.0)), "1");
        assert_eq!(value_to_conf(&Value::String("us".into())), "us");
        assert_eq!(value_to_conf(&Value::Enum("dwindle".into())), "dwindle");
        // Integer "mode" enums serialize verbatim as their numeric literal.
        assert_eq!(value_to_conf(&Value::Enum("2".into())), "2");
        assert_eq!(value_to_conf(&Value::Enum("-1".into())), "-1");
        assert_eq!(
            value_to_conf(&Value::Color(Color::rgba(0x1a, 0x2b, 0x3c, 0xff))),
            "rgba(1a2b3cff)"
        );
        assert_eq!(value_to_conf(&Value::Vec2(Vec2::new(0.0, 0.0))), "0 0");
    }
}

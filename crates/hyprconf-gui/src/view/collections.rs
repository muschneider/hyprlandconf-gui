// SPDX-License-Identifier: MIT OR Apache-2.0
//! The list-shaped parts of a config: keybinds, rules, workspaces, curves,
//! animations, gestures, devices, permissions, plugins, env, exec, …
//!
//! Every entry is a one-line **summary** (what it does, at a glance) that
//! expands into its full editor. A config with seventy keybinds used to be
//! seventy tall cards of form fields; now it is a scannable list with a
//! filter box, and only the entry you are working on takes up room.
//! Short entries (env, exec, plugins, …) are edited inline, no expanding.

use iced::mouse;
use iced::widget::canvas::{Frame, Geometry, Path, Program, Stroke};
use iced::widget::{
    button, canvas as canvas_widget, column, container, pick_list, row, slider, text, text_input,
    toggler, tooltip, Column, Space,
};
use iced::{Alignment, Element, Length, Point, Rectangle, Renderer, Size, Theme};

use hyprconf_core::schema::CollectionId;
use hyprconf_core::structured::{
    Animation, Bezier, Device, EnvVar, Exec, ExecKind, Gesture, Keybind, LayerRule, MonitorRule,
    Permission, Plugin, Submap, Variable, WindowRule, WorkspaceRule, DEVICE_OPTIONS,
    GESTURE_ACTIONS, GESTURE_DIRECTIONS, PERMISSION_MODES, PERMISSION_TYPES,
};

use super::nav::{collection_count, collection_icon};
use super::options::{divider, segmented, EnumChoice};
use super::styles::*;
use super::{empty_note, pane_header_count, scroll, BOLD, MONO};
use crate::edit::{
    device_issue, dispatcher_help, env_issue, exec_issue, extra_field, gesture_issue,
    gesture_shadowed_by, has_mod, keybind_issue, layer_rule_issue, monitor_issue, parse_matchers,
    permission_issue, plugin_issue, variable_issue, window_rule_issue, workspace_issue,
    AnimationEdit, BezierEdit, BindFlag, CollectionAction, DeviceEdit, Dir, EnvEdit, ExecEdit,
    GestureEdit, KeybindEdit, LayerRuleEdit, MonitorEdit, PermissionEdit, VariableEdit,
    WindowRuleEdit, WorkspaceEdit, DISPATCHERS, MODS,
};
use crate::load::Loaded;
use crate::{App, Message};

/// The animation targets Hyprland's animation tree defines.
const ANIMATION_NAMES: &[&str] = &[
    "global",
    "windows",
    "windowsIn",
    "windowsOut",
    "windowsMove",
    "layers",
    "layersIn",
    "layersOut",
    "fade",
    "fadeIn",
    "fadeOut",
    "fadeSwitch",
    "fadeShadow",
    "fadeDim",
    "fadeLayers",
    "fadeLayersIn",
    "fadeLayersOut",
    "fadePopups",
    "fadePopupsIn",
    "fadePopupsOut",
    "fadeDpms",
    "border",
    "borderangle",
    "workspaces",
    "workspacesIn",
    "workspacesOut",
    "specialWorkspace",
    "specialWorkspaceIn",
    "specialWorkspaceOut",
    "zoomFactor",
    "monitorAdded",
];

/// Window-rule (v2) matcher keys, in `.conf` spelling.
const MATCH_KEYS: &[&str] = &[
    "class",
    "title",
    "initialClass",
    "initialTitle",
    "tag",
    "xwayland",
    "floating",
    "fullscreen",
    "pinned",
    "focus",
    "group",
    "modal",
    "fullscreenstate",
    "workspace",
    "onworkspace",
    "content",
    "xdgTag",
];

pub(super) fn collection_view<'a>(
    app: &'a App,
    loaded: &'a Loaded,
    id: CollectionId,
) -> Element<'a, Message> {
    let (label, description) = app
        .schema
        .collection(id)
        .map(|c| (c.label.as_str(), c.description.as_str()))
        .unwrap_or_default();
    let count = collection_count(app, id);

    let mut items: Vec<Element<Message>> = vec![pane_header_count(
        collection_icon(id),
        label,
        description,
        format!("{count} entr{}", if count == 1 { "y" } else { "ies" }),
    )];
    if let Some(note) = collection_note(id, loaded) {
        items.push(note);
    }
    items.push(toolbar(app, id, count));

    let filter = app.collection_filter.trim().to_lowercase();
    let mut shown = 0usize;
    for i in 0..count {
        if !filter.is_empty() && !entry_text(loaded, id, i).to_lowercase().contains(&filter) {
            continue;
        }
        shown += 1;
        items.push(entry(app, loaded, id, i, count));
    }
    if count == 0 {
        items.push(empty_note("Nothing here yet — use the add button above."));
    } else if shown == 0 {
        items.push(empty_note("No entries match the filter."));
    }

    scroll(items)
}

/// A context note shown above certain collections.
fn collection_note(id: CollectionId, loaded: &Loaded) -> Option<Element<'static, Message>> {
    let (message, warn) = match id {
        CollectionId::Permissions => {
            let enforced = matches!(
                loaded.config.get("ecosystem:enforce_permissions"),
                Some(hyprconf_core::Value::Bool(true))
            );
            (
                if enforced {
                    "Permission rules are enforced. Changes take effect after restarting Hyprland. \
                     The first matching rule wins."
                } else {
                    "These rules are ignored until System › Ecosystem › Enforce permissions is on \
                     (and Hyprland is restarted). The first matching rule wins."
                },
                !enforced,
            )
        }
        CollectionId::Gestures => (
            "Earlier gestures win: a later gesture with the same fingers and modifiers is \
             rejected if an earlier one already covers its direction (e.g. horizontal covers left).",
            false,
        ),
        CollectionId::Devices => (
            "Device names come from `hyprctl devices`. Options here override the global Input \
             section for that one device.",
            false,
        ),
        CollectionId::WindowRules => (
            "Rules apply top to bottom. Effects and matchers are offered from the list Hyprland \
             understands, so every rule here also converts to Lua.",
            false,
        ),
        _ => return None,
    };
    let style: fn(&Theme) -> text::Style = if warn { warn_style } else { muted };
    Some(
        container(text(message).size(12).style(style))
            .padding([8, 14])
            .width(Length::Fill)
            .style(inset_style)
            .into(),
    )
}

fn toolbar(app: &App, id: CollectionId, count: usize) -> Element<'_, Message> {
    let add = button(text(format!("+ Add {}", singular(id))).size(13))
        .padding([6, 14])
        .on_press(Message::CollectionEdit(CollectionAction::Add(id)))
        .style(primary_button);
    let mut bar = row![add].spacing(10).align_y(Alignment::Center);
    // Stays while a filter is active, even if the list shrinks below the
    // threshold — otherwise there'd be no way to clear it.
    if count > 5 || !app.collection_filter.is_empty() {
        bar = bar.push(
            text_input("Filter…", &app.collection_filter)
                .on_input(Message::CollectionFilter)
                .padding([6, 10])
                .size(13)
                .width(Length::Fixed(260.0)),
        );
    }
    container(bar).padding([2, 0]).into()
}

fn singular(id: CollectionId) -> &'static str {
    match id {
        CollectionId::Keybinds => "keybind",
        CollectionId::WindowRules => "window rule",
        CollectionId::LayerRules => "layer rule",
        CollectionId::Monitors => "monitor",
        CollectionId::Submaps => "submap",
        CollectionId::Env => "variable",
        CollectionId::Execs => "command",
        CollectionId::Workspaces => "workspace rule",
        CollectionId::Variables => "variable",
        CollectionId::Beziers => "curve",
        CollectionId::Animations => "animation",
        CollectionId::Gestures => "gesture",
        CollectionId::Devices => "device",
        CollectionId::Permissions => "permission",
        CollectionId::Plugins => "plugin",
    }
}

/// Wrap a collection edit into a [`Message`].
fn edit(action: CollectionAction) -> Message {
    Message::CollectionEdit(action)
}

/// One entry: summary line + row controls, expanding into its editor.
fn entry<'a>(
    app: &'a App,
    loaded: &'a Loaded,
    id: CollectionId,
    i: usize,
    count: usize,
) -> Element<'a, Message> {
    let c = &loaded.config;
    let (summary, issue, body): (Element<Message>, Option<String>, Option<Element<Message>>) =
        match id {
            CollectionId::Keybinds => {
                let kb = &c.keybinds[i].value;
                (
                    keybind_summary(kb),
                    keybind_issue(kb),
                    Some(keybind_body(i, kb)),
                )
            }
            CollectionId::WindowRules => {
                let r = &c.window_rules[i].value;
                (
                    rule_summary(&r.rule, &r.matchers, "every window"),
                    window_rule_issue(r),
                    Some(window_rule_body(i, r)),
                )
            }
            CollectionId::LayerRules => {
                let r = &c.layer_rules[i].value;
                (
                    rule_summary(&r.rule, &r.namespace, "every layer"),
                    layer_rule_issue(r),
                    Some(layer_rule_body(i, r)),
                )
            }
            CollectionId::Workspaces => {
                let w = &c.workspaces[i].value;
                (
                    workspace_summary(w),
                    workspace_issue(w),
                    Some(workspace_body(i, w)),
                )
            }
            CollectionId::Beziers => {
                let b = &c.beziers[i].value;
                (bezier_summary(b), None, Some(bezier_body(i, b)))
            }
            CollectionId::Animations => {
                let a = &c.animations[i].value;
                let curves: Vec<String> = c.beziers.iter().map(|b| b.value.name.clone()).collect();
                (
                    animation_summary(a),
                    None,
                    Some(animation_body(i, a, curves)),
                )
            }
            CollectionId::Gestures => {
                let g = &c.gestures[i].value;
                let issue = gesture_issue(g).or_else(|| {
                    gesture_shadowed_by(&c.gestures, i)
                        .map(|j| format!("shadowed by #{} — Hyprland ignores this one", j + 1))
                });
                (gesture_summary(g), issue, Some(gesture_body(i, g)))
            }
            CollectionId::Devices => {
                let d = &c.devices[i].value;
                (device_summary(d), device_issue(d), Some(device_body(i, d)))
            }
            // (The Monitors screen has its own hardware-driven view; this arm
            // only matters if a monitor list is ever shown generically.)
            CollectionId::Monitors => {
                let m = &c.monitors[i].value;
                (
                    text(monitor_line(m)).size(13).into(),
                    monitor_issue(m),
                    Some(monitor_fields(i, m)),
                )
            }
            CollectionId::Env => {
                let e = &c.env[i].value;
                (env_inline(i, e), env_issue(e), None)
            }
            CollectionId::Execs => {
                let e = &c.execs[i].value;
                (exec_inline(i, e), exec_issue(e), None)
            }
            CollectionId::Variables => {
                let v = &c.variables[i].value;
                (variable_inline(i, v), variable_issue(v), None)
            }
            CollectionId::Submaps => {
                let s = &c.submaps[i].value;
                (submap_inline(i, s), None, None)
            }
            CollectionId::Permissions => {
                let p = &c.permissions[i].value;
                (permission_inline(i, p), permission_issue(p), None)
            }
            CollectionId::Plugins => {
                let p = &c.plugins[i].value;
                (plugin_inline(i, p), plugin_issue(p), None)
            }
        };

    let expanded = body.is_some() && app.expanded_rows.contains(&(id, i));
    let summary: Element<Message> = if body.is_some() {
        // The whole summary is the expand target — the biggest, most obvious
        // place to click.
        button(summary)
            .width(Length::Fill)
            .padding([2, 4])
            .on_press(Message::ToggleRow(id, i))
            .style(ghost_button)
            .into()
    } else {
        container(summary).width(Length::Fill).into()
    };

    let mut header = row![
        container(text(format!("{}", i + 1)).size(11).style(muted)).width(Length::Fixed(22.0)),
        summary,
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if let Some(issue) = issue {
        header = header
            .push(container(text(format!("✕ {issue}")).size(11).style(danger)).max_width(260.0));
    }
    header = header.push(row_controls(id, i, count));
    if body.is_some() {
        header = header.push(icon_button(
            if expanded { "▴" } else { "▾" },
            if expanded { "Collapse" } else { "Edit" },
            Some(Message::ToggleRow(id, i)),
        ));
    }

    let mut card = column![header].spacing(8);
    if let (true, Some(body)) = (expanded, body) {
        card = card.push(divider()).push(container(body).padding([4, 6]));
    }
    container(card)
        .padding([6, 10])
        .width(Length::Fill)
        .style(card_style)
        .into()
}

/// Reorder / duplicate / remove.
fn row_controls(id: CollectionId, i: usize, count: usize) -> Element<'static, Message> {
    row![
        icon_button(
            "↑",
            "Move up (earlier entries apply first)",
            (i > 0).then(|| edit(CollectionAction::Move(id, i, Dir::Up)))
        ),
        icon_button(
            "↓",
            "Move down",
            (i + 1 < count).then(|| edit(CollectionAction::Move(id, i, Dir::Down)))
        ),
        icon_button(
            "⧉",
            "Duplicate",
            Some(edit(CollectionAction::Duplicate(id, i)))
        ),
        icon_button("✕", "Remove", Some(edit(CollectionAction::Remove(id, i)))),
    ]
    .spacing(0)
    .align_y(Alignment::Center)
    .into()
}

fn icon_button(
    glyph: &'static str,
    tip: &'static str,
    message: Option<Message>,
) -> Element<'static, Message> {
    let mut b = button(text(glyph).size(13))
        .padding([2, 7])
        .style(ghost_button);
    if let Some(m) = message {
        b = b.on_press(m);
    }
    with_tip(b, tip)
}

fn with_tip<'a>(content: impl Into<Element<'a, Message>>, tip: &'a str) -> Element<'a, Message> {
    tooltip(
        content,
        container(text(tip).size(12))
            .padding([6, 10])
            .max_width(320.0)
            .style(tooltip_style),
        tooltip::Position::Top,
    )
    .into()
}

/// A small caption above a field.
fn labeled<'a>(
    label: &'static str,
    field: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    column![text(label).size(11).style(muted), field.into()]
        .spacing(3)
        .into()
}

/// A plain text input for a collection field.
pub(crate) fn coll_text(
    value: &str,
    placeholder: &str,
    width: Length,
    make: impl Fn(String) -> Message + 'static,
) -> Element<'static, Message> {
    text_input(placeholder, value)
        .on_input(make)
        .padding([6, 8])
        .size(13)
        .width(width)
        .into()
}

pub(crate) fn chip(label: String, active: bool, message: Message) -> Element<'static, Message> {
    button(text(label).size(12))
        .padding([3, 9])
        .on_press(message)
        .style(move |theme: &Theme, status| chip_style(theme, status, active))
        .into()
}

/// A dropdown over `(value, description)` pairs that keeps an unknown current
/// value selectable rather than hiding it.
fn choice_list<'a>(
    options: impl IntoIterator<Item = (&'a str, &'a str)>,
    current: &str,
    width: Length,
    on_select: impl Fn(String) -> Message + 'static,
) -> Element<'static, Message> {
    let mut choices: Vec<EnumChoice> = options
        .into_iter()
        .map(|(value, desc)| EnumChoice {
            value: value.to_string(),
            label: if desc.is_empty() {
                value.to_string()
            } else {
                format!("{value}  ·  {desc}")
            },
        })
        .collect();
    if !current.is_empty() && !choices.iter().any(|c| c.value == current) {
        choices.insert(
            0,
            EnumChoice {
                value: current.to_string(),
                label: current.to_string(),
            },
        );
    }
    let selected = choices.iter().find(|c| c.value == current).cloned();
    pick_list(choices, selected, move |c: EnumChoice| on_select(c.value))
        .padding([6, 8])
        .text_size(13)
        .width(width)
        .into()
}

fn keycap(label: &str) -> Element<'static, Message> {
    container(text(label.to_string()).size(11).font(MONO))
        .padding([1, 6])
        .style(keycap_style)
        .into()
}

fn tag(label: String) -> Element<'static, Message> {
    container(text(label).size(10))
        .padding([1, 6])
        .style(soft_badge_style)
        .into()
}

/// Split a rule body into its effect and argument: `opacity 0.9` → (`opacity`, `0.9`).
fn split_rule(rule: &str) -> (&str, &str) {
    let rule = rule.trim();
    match rule.split_once(char::is_whitespace) {
        Some((head, rest)) => (head, rest.trim()),
        None => (rule, ""),
    }
}

fn join_rule(effect: &str, arg: &str) -> String {
    if arg.trim().is_empty() {
        effect.to_string()
    } else {
        format!("{effect} {}", arg.trim())
    }
}

// ---------------------------------------------------------------------------
// keybinds
// ---------------------------------------------------------------------------

fn keybind_summary(kb: &Keybind) -> Element<'static, Message> {
    let mut keys = row![].spacing(3).align_y(Alignment::Center);
    for m in kb
        .mods
        .split(|c: char| c.is_whitespace() || c == '+')
        .filter(|s| !s.is_empty())
    {
        keys = keys.push(keycap(m));
    }
    keys = keys.push(keycap(if kb.key.is_empty() { "?" } else { &kb.key }));

    let action = if kb.args.is_empty() {
        kb.dispatcher.clone()
    } else {
        format!("{} {}", kb.dispatcher, kb.args)
    };
    let mut line = row![
        keys,
        text("→").size(12).style(muted),
        text(action).size(13).font(MONO),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let flags: Vec<&str> = BindFlag::ALL
        .iter()
        .filter(|(f, _, _)| f.is_set(&kb.flags))
        .map(|(_, label, _)| *label)
        .collect();
    if !flags.is_empty() {
        line = line.push(tag(flags.join(" · ")));
    }
    if let Some(submap) = &kb.submap {
        line = line.push(tag(format!("submap {submap}")));
    }
    match &kb.description {
        Some(d) => column![line, text(d.clone()).size(11).style(muted)]
            .spacing(2)
            .into(),
        None => line.into(),
    }
}

fn keybind_body(i: usize, kb: &Keybind) -> Element<'static, Message> {
    let msg = move |e: KeybindEdit| edit(CollectionAction::Keybind(i, e));

    let mods = row(MODS.iter().map(|&name| {
        let active = has_mod(&kb.mods, name);
        chip(
            name.to_string(),
            active,
            msg(KeybindEdit::ToggleMod(name.to_string(), !active)),
        )
    }))
    .spacing(4);

    let key = coll_text(
        &kb.key,
        "Q, Return, mouse:272, code:24…",
        Length::Fixed(190.0),
        move |s| msg(KeybindEdit::Key(s)),
    );
    let dispatcher = choice_list(
        DISPATCHERS
            .iter()
            .map(|(name, help)| (*name, help.split(" · ").next().unwrap_or(help))),
        &kb.dispatcher,
        Length::Fixed(300.0),
        move |d| msg(KeybindEdit::Dispatcher(d)),
    );
    let help = dispatcher_help(&kb.dispatcher);
    let args_hint = help.and_then(|h| h.split_once("args: ")).map(|(_, a)| a);
    let args = coll_text(
        &kb.args,
        args_hint.unwrap_or("no arguments"),
        Length::Fill,
        move |s| msg(KeybindEdit::Args(s)),
    );
    // The picker already says what the dispatcher does; below it, say what
    // the arguments look like (the placeholder vanishes once you type).
    let help_line = match (help, args_hint) {
        (None, _) if !kb.dispatcher.is_empty() => {
            "Not a dispatcher hyprconf knows — kept as written (it may come from a plugin)."
                .to_string()
        }
        (_, Some(args)) => format!("Arguments: {args}"),
        _ => String::new(),
    };

    let flags = row(BindFlag::ALL.iter().map(|(flag, label, tip)| {
        let active = flag.is_set(&kb.flags);
        with_tip(
            chip(
                (*label).to_string(),
                active,
                msg(KeybindEdit::Flag(*flag, !active)),
            ),
            tip,
        )
    }))
    .spacing(4)
    .wrap();

    let mut body = column![
        row![labeled("Modifiers", mods), labeled("Key", key)]
            .spacing(16)
            .align_y(Alignment::End),
        row![
            labeled("Action", dispatcher),
            column![text("Arguments").size(11).style(muted), args]
                .spacing(3)
                .width(Length::Fill),
        ]
        .spacing(12),
    ]
    .spacing(10);
    if !help_line.is_empty() {
        body = body.push(text(help_line).size(11).style(muted));
    }
    body.push(
        row![
            column![
                text("Description").size(11).style(muted),
                coll_text(
                    kb.description.as_deref().unwrap_or(""),
                    "What this shortcut does — shown by hyprctl binds and helper tools",
                    Length::Fill,
                    move |s| msg(KeybindEdit::Description(s)),
                ),
            ]
            .spacing(3)
            .width(Length::Fill),
            labeled(
                "Submap",
                coll_text(
                    kb.submap.as_deref().unwrap_or(""),
                    "global",
                    Length::Fixed(150.0),
                    move |s| msg(KeybindEdit::Submap(s)),
                ),
            ),
        ]
        .spacing(12),
    )
    .push(labeled("Behaviour", flags))
    .into()
}

// ---------------------------------------------------------------------------
// window / layer rules
// ---------------------------------------------------------------------------

fn rule_summary(rule: &str, target: &str, everything: &str) -> Element<'static, Message> {
    let (effect, arg) = split_rule(rule);
    let mut line = row![text(effect.to_string()).size(13).font(BOLD)]
        .spacing(8)
        .align_y(Alignment::Center);
    if !arg.is_empty() {
        line = line.push(text(arg.to_string()).size(13).font(MONO));
    }
    line = line.push(text("for").size(12).style(muted));
    line.push(
        text(if target.trim().is_empty() {
            everything.to_string()
        } else {
            target.to_string()
        })
        .size(12)
        .font(MONO)
        .style(muted),
    )
    .into()
}

fn window_rule_body(i: usize, wr: &WindowRule) -> Element<'static, Message> {
    let msg = move |e: WindowRuleEdit| edit(CollectionAction::WindowRule(i, e));
    let (effect, arg) = split_rule(&wr.rule);
    let (effect, arg) = (effect.to_string(), arg.to_string());
    let effects = hyprconf_core::lua::window_rule_effects();
    let takes_arg = effects.iter().any(|(name, a)| *name == effect && *a) || !arg.is_empty();

    let arg_for_effect = arg.clone();
    let effect_picker = choice_list(
        effects
            .iter()
            .map(|(name, a)| (*name, if *a { "takes a value" } else { "" })),
        &effect,
        Length::Fixed(240.0),
        move |e| msg(WindowRuleEdit::Rule(join_rule(&e, &arg_for_effect))),
    );
    let mut top = row![labeled("Effect", effect_picker)]
        .spacing(12)
        .align_y(Alignment::End);
    if takes_arg {
        let effect_for_arg = effect.clone();
        top = top.push(
            column![
                text("Value").size(11).style(muted),
                coll_text(
                    &arg,
                    "e.g. 0.9 0.8, 800 600, 2 silent",
                    Length::Fill,
                    move |s| { msg(WindowRuleEdit::Rule(join_rule(&effect_for_arg, &s))) }
                ),
            ]
            .spacing(3)
            .width(Length::Fill),
        );
    }

    let mut matches = Column::new().spacing(6);
    for (mi, (key, value)) in parse_matchers(&wr.matchers).into_iter().enumerate() {
        matches = matches.push(
            row![
                choice_list(
                    MATCH_KEYS.iter().map(|k| (*k, "")),
                    &key,
                    Length::Fixed(170.0),
                    move |k| msg(WindowRuleEdit::MatchKey(mi, k)),
                ),
                text("=").size(13).style(muted),
                coll_text(&value, "regex, e.g. ^(kitty)$", Length::Fill, move |s| {
                    msg(WindowRuleEdit::MatchValue(mi, s))
                }),
                icon_button(
                    "✕",
                    "Remove this condition",
                    Some(msg(WindowRuleEdit::RemoveMatch(mi)))
                ),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
    }

    column![
        top,
        text("Applies to windows matching all of:")
            .size(11)
            .style(muted),
        matches,
        row![
            button(text("+ condition").size(12))
                .padding([4, 10])
                .on_press(msg(WindowRuleEdit::AddMatch))
                .style(ghost_button),
            Space::new().width(Length::Fill),
            chip(
                "v2 matchers".to_string(),
                wr.v2,
                msg(WindowRuleEdit::V2(!wr.v2)),
            ),
        ]
        .align_y(Alignment::Center),
        labeled(
            "Raw matchers",
            coll_text(
                &wr.matchers,
                "class:^(kitty)$, title:…",
                Length::Fill,
                move |s| { msg(WindowRuleEdit::Matchers(s)) }
            ),
        ),
    ]
    .spacing(8)
    .into()
}

fn layer_rule_body(i: usize, lr: &LayerRule) -> Element<'static, Message> {
    let msg = move |e: LayerRuleEdit| edit(CollectionAction::LayerRule(i, e));
    let (effect, arg) = split_rule(&lr.rule);
    let (effect, arg) = (effect.to_string(), arg.to_string());
    let effects = hyprconf_core::lua::layer_rule_effects();
    let arg_for_effect = arg.clone();
    let effect_for_arg = effect.clone();
    row![
        labeled(
            "Effect",
            choice_list(
                effects
                    .iter()
                    .map(|(name, a)| (*name, if *a { "takes a value" } else { "" })),
                &effect,
                Length::Fixed(200.0),
                move |e| msg(LayerRuleEdit::Rule(join_rule(&e, &arg_for_effect))),
            ),
        ),
        labeled(
            "Value",
            coll_text(&arg, "e.g. 0.5", Length::Fixed(110.0), move |s| {
                msg(LayerRuleEdit::Rule(join_rule(&effect_for_arg, &s)))
            }),
        ),
        column![
            text("Layer namespace").size(11).style(muted),
            coll_text(
                &lr.namespace,
                "waybar, notifications, ^(rofi)$",
                Length::Fill,
                move |s| { msg(LayerRuleEdit::Namespace(s)) }
            ),
        ]
        .spacing(3)
        .width(Length::Fill),
    ]
    .spacing(12)
    .into()
}

// ---------------------------------------------------------------------------
// workspaces
// ---------------------------------------------------------------------------

fn workspace_summary(w: &WorkspaceRule) -> Element<'static, Message> {
    row![
        text(if w.selector.is_empty() {
            "(no workspace)".to_string()
        } else {
            w.selector.clone()
        })
        .size(13)
        .font(BOLD),
        text(w.rules.clone()).size(12).font(MONO).style(muted),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

fn workspace_body(i: usize, w: &WorkspaceRule) -> Element<'static, Message> {
    let msg = move |e: WorkspaceEdit| edit(CollectionAction::Workspace(i, e));
    let keys = hyprconf_core::lua::workspace_rule_keys();
    let mut rules = Column::new().spacing(6);
    for (ri, (key, value)) in parse_matchers(&w.rules).into_iter().enumerate() {
        rules = rules.push(
            row![
                choice_list(
                    keys.iter().map(|k| (*k, "")),
                    &key,
                    Length::Fixed(190.0),
                    move |k| msg(WorkspaceEdit::RuleKey(ri, k)),
                ),
                text(":").size(13).style(muted),
                coll_text(&value, "value", Length::Fill, move |s| {
                    msg(WorkspaceEdit::RuleValue(ri, s))
                }),
                icon_button(
                    "✕",
                    "Remove this rule",
                    Some(msg(WorkspaceEdit::RemoveRule(ri)))
                ),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
    }
    column![
        labeled(
            "Workspace",
            coll_text(
                &w.selector,
                "1, name:web, special:magic, r[1-5], m[DP-1]",
                Length::Fixed(320.0),
                move |s| msg(WorkspaceEdit::Selector(s)),
            ),
        ),
        text("Rules").size(11).style(muted),
        rules,
        button(text("+ rule").size(12))
            .padding([4, 10])
            .on_press(msg(WorkspaceEdit::AddRule))
            .style(ghost_button),
        labeled(
            "Raw rules",
            coll_text(
                &w.rules,
                "monitor:DP-1, default:true",
                Length::Fill,
                move |s| { msg(WorkspaceEdit::Rules(s)) }
            ),
        ),
    ]
    .spacing(8)
    .into()
}

// ---------------------------------------------------------------------------
// curves & animations
// ---------------------------------------------------------------------------

fn bezier_summary(b: &Bezier) -> Element<'static, Message> {
    row![
        curve_preview(b, 30.0),
        text(b.name.clone()).size(13).font(BOLD),
        text(format!(
            "({}, {})  ({}, {})",
            b.p0.x, b.p0.y, b.p1.x, b.p1.y
        ))
        .size(12)
        .font(MONO)
        .style(muted),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

fn bezier_body(i: usize, b: &Bezier) -> Element<'static, Message> {
    let msg = move |e: BezierEdit| edit(CollectionAction::Bezier(i, e));
    let coord = |index: usize, label: &'static str, value: f64, range: (f64, f64)| {
        row![
            text(label).size(12).style(muted).width(Length::Fixed(24.0)),
            slider(range.0..=range.1, value.clamp(range.0, range.1), move |v| {
                msg(BezierEdit::Coord(index, (v * 100.0).round() / 100.0))
            })
            .step(0.01)
            .width(Length::Fill),
            text(format!("{value:.2}"))
                .size(12)
                .font(MONO)
                .width(Length::Fixed(44.0)),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
    };
    row![
        column![
            labeled(
                "Name",
                coll_text(&b.name, "myCurve", Length::Fixed(220.0), move |s| {
                    msg(BezierEdit::Name(s))
                }),
            ),
            text("First control point").size(11).style(muted),
            coord(0, "x", b.p0.x, (0.0, 1.0)),
            coord(1, "y", b.p0.y, (-1.0, 2.0)),
            text("Second control point").size(11).style(muted),
            coord(2, "x", b.p1.x, (0.0, 1.0)),
            coord(3, "y", b.p1.y, (-1.0, 2.0)),
        ]
        .spacing(6)
        .width(Length::Fill),
        curve_preview(b, 150.0),
    ]
    .spacing(18)
    .align_y(Alignment::Center)
    .into()
}

/// A drawing of a bezier easing curve (time →, progress ↑), with its handles.
fn curve_preview(b: &Bezier, size: f32) -> Element<'static, Message> {
    canvas_widget(CurvePreview {
        p0: (b.p0.x as f32, b.p0.y as f32),
        p1: (b.p1.x as f32, b.p1.y as f32),
        detailed: size > 60.0,
    })
    .width(Length::Fixed(size))
    .height(Length::Fixed(size))
    .into()
}

#[derive(Debug, Clone, Copy)]
struct CurvePreview {
    p0: (f32, f32),
    p1: (f32, f32),
    detailed: bool,
}

impl Program<Message> for CurvePreview {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let palette = theme.extended_palette();
        let (w, h) = (bounds.width, bounds.height);
        // Leave room for overshoot: y in -0.25..=1.25.
        let map = |x: f32, y: f32| Point::new(x * w, h - (y + 0.25) / 1.5 * h);

        let (start, end) = (map(0.0, 0.0), map(1.0, 1.0));
        frame.stroke(
            &Path::rectangle(map(0.0, 1.0), Size::new(w, h / 1.5)),
            Stroke::default()
                .with_color(palette.background.strong.color)
                .with_width(1.0),
        );
        let (c0, c1) = (map(self.p0.0, self.p0.1), map(self.p1.0, self.p1.1));
        if self.detailed {
            let handle = Stroke::default()
                .with_color(palette.background.base.text.scale_alpha(0.35))
                .with_width(1.0);
            frame.stroke(&Path::line(start, c0), handle);
            frame.stroke(&Path::line(end, c1), handle);
            frame.fill(&Path::circle(c0, 3.5), palette.primary.weak.color);
            frame.fill(&Path::circle(c1, 3.5), palette.primary.weak.color);
        }
        let curve = Path::new(|p| {
            p.move_to(start);
            p.bezier_curve_to(c0, c1, end);
        });
        frame.stroke(
            &curve,
            Stroke::default()
                .with_color(palette.primary.base.color)
                .with_width(if self.detailed { 2.5 } else { 1.5 }),
        );
        vec![frame.into_geometry()]
    }
}

fn animation_summary(a: &Animation) -> Element<'static, Message> {
    let mut line = row![
        text(if a.enabled { "●" } else { "○" })
            .size(11)
            .style(if a.enabled { success } else { muted }),
        text(a.name.clone()).size(13).font(BOLD),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if a.enabled {
        line = line.push(
            text(format!("{} ds · {}", a.speed, a.curve))
                .size(12)
                .style(muted),
        );
        if let Some(style) = &a.style {
            line = line.push(tag(style.clone()));
        }
    } else {
        line = line.push(text("off").size(12).style(muted));
    }
    line.into()
}

fn animation_body(i: usize, a: &Animation, curves: Vec<String>) -> Element<'static, Message> {
    let msg = move |e: AnimationEdit| edit(CollectionAction::Animation(i, e));
    let mut curve_names: Vec<String> = vec!["default".to_string()];
    for c in curves {
        if !curve_names.contains(&c) {
            curve_names.push(c);
        }
    }
    let speed = a.speed;
    column![
        row![
            labeled(
                "Target",
                choice_list(
                    ANIMATION_NAMES.iter().map(|n| (*n, "")),
                    &a.name,
                    Length::Fixed(220.0),
                    move |s| msg(AnimationEdit::Name(s)),
                ),
            ),
            labeled(
                "Enabled",
                toggler(a.enabled)
                    .on_toggle(move |on| msg(AnimationEdit::Enabled(on)))
                    .size(20),
            ),
            labeled(
                "Curve",
                choice_list(
                    curve_names.iter().map(|c| (c.as_str(), "")),
                    &a.curve,
                    Length::Fixed(200.0),
                    move |s| msg(AnimationEdit::Curve(s)),
                ),
            ),
        ]
        .spacing(16)
        .align_y(Alignment::End),
        labeled(
            "Duration (deciseconds — 10 = 1 s)",
            row![
                slider(0.1..=30.0, speed.clamp(0.1, 30.0), move |v| msg(
                    AnimationEdit::Speed(v)
                ))
                .step(0.1)
                .width(Length::Fill),
                text(format!("{speed:.1}"))
                    .size(12)
                    .font(MONO)
                    .width(Length::Fixed(40.0)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ),
        labeled(
            "Style",
            coll_text(
                a.style.as_deref().unwrap_or(""),
                "slide, popin 80%, gnomed, slidevert, slidefade 20%, fade, once, loop…",
                Length::Fill,
                move |s| msg(AnimationEdit::Style(s)),
            ),
        ),
    ]
    .spacing(10)
    .into()
}

// ---------------------------------------------------------------------------
// gestures & devices
// ---------------------------------------------------------------------------

fn gesture_summary(g: &Gesture) -> Element<'static, Message> {
    let mut line = row![].spacing(6).align_y(Alignment::Center);
    for m in g.mods.split_whitespace() {
        line = line.push(keycap(m));
    }
    line = line.push(
        text(format!("{} fingers · {}", g.fingers, g.direction))
            .size(13)
            .font(BOLD),
    );
    line = line.push(text("→").size(12).style(muted));
    let action = if g.args.is_empty() {
        g.action.clone()
    } else {
        format!("{} {}", g.action, g.args)
    };
    line = line.push(text(action).size(13).font(MONO));
    if let Some(scale) = g.scale {
        line = line.push(tag(format!("scale {scale}")));
    }
    if g.bypass_inhibit {
        line = line.push(tag("bypasses inhibitors".to_string()));
    }
    line.into()
}

fn gesture_body(i: usize, g: &Gesture) -> Element<'static, Message> {
    let msg = move |e: GestureEdit| edit(CollectionAction::Gesture(i, e));
    let fingers = segmented(
        (2..=5)
            .map(|n| (n.to_string(), n.to_string(), None))
            .collect(),
        &g.fingers.to_string(),
        move |n| msg(GestureEdit::Fingers(n.parse().unwrap_or(3))),
    );
    let mods = row(MODS.iter().map(|&name| {
        let active = has_mod(&g.mods, name);
        chip(
            name.to_string(),
            active,
            msg(GestureEdit::ToggleMod(name.to_string(), !active)),
        )
    }))
    .spacing(4);
    let help = GESTURE_ACTIONS
        .iter()
        .find(|(a, _)| *a == g.action)
        .map_or("", |(_, h)| *h);
    let args_hint = match g.action.as_str() {
        "special" => Some("workspace name, e.g. magic"),
        "fullscreen" => Some("maximize (optional)"),
        "float" => Some("float or tile (optional)"),
        "cursorZoom" => Some("factor[, mult | live], e.g. 1.5, mult"),
        "dispatcher" => Some("dispatcher, params — e.g. workspace, e+1"),
        // Unknown actions keep a field so nothing written there is hidden.
        _ if !GESTURE_ACTIONS.iter().any(|(a, _)| *a == g.action) || !g.args.is_empty() => {
            Some("arguments")
        }
        _ => None,
    };
    let mut action_row = row![labeled(
        "Action",
        choice_list(
            // "Toggle fullscreen — optional argument: maximize" → "Toggle fullscreen".
            GESTURE_ACTIONS.iter().map(|(a, h)| {
                let short = h.split(" — ").next().unwrap_or(h);
                (*a, short.split(" (").next().unwrap_or(short))
            }),
            &g.action,
            Length::Fixed(340.0),
            move |a| msg(GestureEdit::Action(a)),
        ),
    )]
    .spacing(12);
    if let Some(hint) = args_hint {
        action_row = action_row.push(
            column![
                text("Arguments").size(11).style(muted),
                coll_text(&g.args, hint, Length::Fill, move |s| {
                    msg(GestureEdit::Args(s))
                }),
            ]
            .spacing(3)
            .width(Length::Fill),
        );
    }

    column![
        row![
            labeled("Fingers", fingers),
            labeled(
                "Direction",
                choice_list(
                    GESTURE_DIRECTIONS.iter().copied(),
                    &g.direction,
                    Length::Fixed(240.0),
                    move |d| msg(GestureEdit::Direction(d)),
                ),
            ),
            labeled("Hold modifiers", mods),
        ]
        .spacing(16)
        .align_y(Alignment::End),
        action_row,
        text(help).size(11).style(muted),
        row![
            toggler(g.bypass_inhibit)
                .on_toggle(move |on| msg(GestureEdit::Bypass(on)))
                .size(18),
            text("Work even while an app inhibits shortcuts (gesturep)")
                .size(12)
                .style(muted),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(10)
    .into()
}

fn device_summary(d: &Device) -> Element<'static, Message> {
    let overrides = d
        .options
        .iter()
        .map(|(k, v)| format!("{k} = {v}"))
        .collect::<Vec<_>>()
        .join(", ");
    row![
        text(if d.name.is_empty() {
            "(unnamed device)".to_string()
        } else {
            d.name.clone()
        })
        .size(13)
        .font(MONO),
        text(overrides).size(12).style(muted),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

fn device_body(i: usize, d: &Device) -> Element<'static, Message> {
    let msg = move |e: DeviceEdit| edit(CollectionAction::Device(i, e));
    let mut options = Column::new().spacing(6);
    for (oi, (key, value)) in d.options.iter().enumerate() {
        let hint = DEVICE_OPTIONS
            .iter()
            .find(|(k, _)| k == key)
            .map_or("value", |(_, h)| *h);
        options = options.push(
            row![
                choice_list(
                    DEVICE_OPTIONS.iter().map(|(k, _)| (*k, "")),
                    key,
                    Length::Fixed(230.0),
                    move |k| msg(DeviceEdit::OptionKey(oi, k)),
                ),
                text("=").size(13).style(muted),
                coll_text(value, hint, Length::Fill, move |s| {
                    msg(DeviceEdit::OptionValue(oi, s))
                }),
                icon_button(
                    "✕",
                    "Remove this override",
                    Some(msg(DeviceEdit::RemoveOption(oi)))
                ),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
    }
    column![
        labeled(
            "Device name",
            coll_text(
                &d.name,
                "as listed by hyprctl devices, e.g. logitech-g502",
                Length::Fixed(360.0),
                move |s| msg(DeviceEdit::Name(s)),
            ),
        ),
        text("Overrides").size(11).style(muted),
        options,
        button(text("+ override").size(12))
            .padding([4, 10])
            .on_press(msg(DeviceEdit::AddOption))
            .style(ghost_button),
    ]
    .spacing(8)
    .into()
}

// ---------------------------------------------------------------------------
// inline editors (short entries)
// ---------------------------------------------------------------------------

fn env_inline(i: usize, e: &EnvVar) -> Element<'static, Message> {
    row![
        coll_text(&e.name, "NAME", Length::Fixed(240.0), move |s| {
            edit(CollectionAction::Env(i, EnvEdit::Name(s)))
        }),
        text("=").size(13).style(muted),
        coll_text(&e.value, "value", Length::Fill, move |s| {
            edit(CollectionAction::Env(i, EnvEdit::Value(s)))
        }),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn exec_inline(i: usize, e: &Exec) -> Element<'static, Message> {
    let kinds = [
        ("exec-once", "at startup", ExecKind::ExecOnce),
        ("exec", "on every reload", ExecKind::Exec),
        ("exec-shutdown", "on exit", ExecKind::ExecShutdown),
    ];
    let current = kinds
        .iter()
        .find(|(_, _, k)| *k == e.kind)
        .map_or("exec-once", |(n, _, _)| *n);
    let kind = segmented(
        kinds
            .iter()
            .map(|(n, label, _)| (n.to_string(), (*label).to_string(), Some(n.to_string())))
            .collect(),
        current,
        move |n| {
            let kind = match n.as_str() {
                "exec" => ExecKind::Exec,
                "exec-shutdown" => ExecKind::ExecShutdown,
                _ => ExecKind::ExecOnce,
            };
            edit(CollectionAction::Exec(i, ExecEdit::Kind(kind)))
        },
    );
    row![
        kind,
        coll_text(&e.command, "command, e.g. waybar", Length::Fill, move |s| {
            edit(CollectionAction::Exec(i, ExecEdit::Command(s)))
        }),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn variable_inline(i: usize, v: &Variable) -> Element<'static, Message> {
    row![
        text("$").size(14).font(MONO).style(muted),
        coll_text(&v.name, "name", Length::Fixed(200.0), move |s| {
            edit(CollectionAction::Variable(i, VariableEdit::Name(s)))
        }),
        text("=").size(13).style(muted),
        coll_text(&v.value, "value", Length::Fill, move |s| {
            edit(CollectionAction::Variable(i, VariableEdit::Value(s)))
        }),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn submap_inline(i: usize, s: &Submap) -> Element<'static, Message> {
    row![
        text("submap").size(12).style(muted),
        coll_text(&s.name, "name (or reset)", Length::Fixed(260.0), move |v| {
            edit(CollectionAction::Submap(i, v))
        }),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn permission_inline(i: usize, p: &Permission) -> Element<'static, Message> {
    let msg = move |e: PermissionEdit| edit(CollectionAction::Permission(i, e));
    row![
        coll_text(
            &p.binary,
            "binary regex, e.g. /usr/bin/grim",
            Length::Fill,
            move |s| msg(PermissionEdit::Binary(s)),
        ),
        choice_list(
            PERMISSION_TYPES.iter().copied(),
            &p.kind,
            Length::Fixed(250.0),
            move |s| msg(PermissionEdit::Kind(s)),
        ),
        segmented(
            PERMISSION_MODES
                .iter()
                .map(|(m, d)| (m.to_string(), m.to_string(), Some(d.to_string())))
                .collect(),
            &p.mode,
            move |s| msg(PermissionEdit::Mode(s)),
        ),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn plugin_inline(i: usize, p: &Plugin) -> Element<'static, Message> {
    coll_text(
        &p.path,
        "/absolute/path/to/plugin.so",
        Length::Fill,
        move |s| edit(CollectionAction::Plugin(i, s)),
    )
}

// ---------------------------------------------------------------------------
// monitors (the raw-rule fallback used by the Monitors screen)
// ---------------------------------------------------------------------------

fn monitor_line(m: &MonitorRule) -> String {
    let name = if m.name.is_empty() { "(all)" } else { &m.name };
    let extra = if m.extra.is_empty() {
        String::new()
    } else {
        format!("  {}", m.extra.join(", "))
    };
    format!("{name}: {} @ {} ×{}{extra}", m.mode, m.position, m.scale)
}

/// A labelled dropdown for a fixed-choice monitor field. Always offers an
/// explicit "(unset)" entry and keeps any unknown current value selectable.
fn coll_choice(
    label: &'static str,
    current: String,
    variants: &[(&str, &str)],
    width: f32,
    on_select: impl Fn(String) -> Message + 'static,
) -> Element<'static, Message> {
    let mut choices = vec![EnumChoice {
        value: String::new(),
        label: "(unset)".to_string(),
    }];
    for (value, desc) in variants {
        choices.push(EnumChoice {
            value: (*value).to_string(),
            label: format!("{value}  ·  {desc}"),
        });
    }
    if !choices.iter().any(|c| c.value == current) {
        choices.push(EnumChoice {
            value: current.clone(),
            label: current.clone(),
        });
    }
    let selected = choices.iter().find(|c| c.value == current).cloned();
    column![
        text(label).size(11).style(muted),
        pick_list(choices, selected, move |c: EnumChoice| on_select(c.value))
            .padding([6, 8])
            .text_size(13)
            .width(Length::Fixed(width)),
    ]
    .spacing(3)
    .width(Length::Fixed(width))
    .into()
}

/// A raw monitor rule as a full card — summary, reorder/duplicate/remove and
/// the field editor — for the Monitors screen's "Other rules" list (wildcards,
/// EDID selectors, unplugged displays).
pub(crate) fn monitor_row(i: usize, m: &MonitorRule, count: usize) -> Element<'static, Message> {
    let id = CollectionId::Monitors;
    let mut header = row![
        container(text(format!("{}", i + 1)).size(11).style(muted)).width(Length::Fixed(22.0)),
        container(text(monitor_line(m)).size(13).font(MONO)).width(Length::Fill),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if let Some(issue) = monitor_issue(m) {
        header = header.push(text(format!("✕ {issue}")).size(11).style(danger));
    }
    header = header.push(row_controls(id, i, count));
    container(column![header, divider(), monitor_fields(i, m)].spacing(8))
        .padding([6, 10])
        .width(Length::Fill)
        .style(card_style)
        .into()
}

/// The raw monitor rule fields (connector, mode, position, scale, extras).
fn monitor_fields(i: usize, m: &MonitorRule) -> Element<'static, Message> {
    let field = |label: &'static str,
                 value: String,
                 placeholder: &'static str,
                 make: fn(String) -> MonitorEdit| {
        column![
            text(label).size(11).style(muted),
            text_input(placeholder, &value)
                .on_input(move |s| edit(CollectionAction::Monitor(i, make(s))))
                .padding([6, 8])
                .size(13),
        ]
        .spacing(3)
    };

    column![
        row![
            field(
                "Connector",
                m.name.clone(),
                "DP-1 / desc:…",
                MonitorEdit::Name
            )
            .width(Length::FillPortion(2)),
            field("Mode", m.mode.clone(), "1920x1080@144", MonitorEdit::Mode)
                .width(Length::FillPortion(2)),
        ]
        .spacing(10),
        row![
            field(
                "Position",
                m.position.clone(),
                "0x0 / auto",
                MonitorEdit::Position
            )
            .width(Length::FillPortion(2)),
            field("Scale", m.scale.clone(), "1 / auto", MonitorEdit::Scale)
                .width(Length::Fixed(110.0)),
            coll_choice(
                "Transform",
                extra_field(&m.extra, "transform"),
                &[
                    ("0", "Normal"),
                    ("1", "90°"),
                    ("2", "180°"),
                    ("3", "270°"),
                    ("4", "Flipped"),
                    ("5", "Flipped + 90°"),
                    ("6", "Flipped + 180°"),
                    ("7", "Flipped + 270°"),
                ],
                170.0,
                move |s| edit(CollectionAction::Monitor(i, MonitorEdit::Transform(s))),
            ),
            coll_choice(
                "VRR",
                extra_field(&m.extra, "vrr"),
                &[("0", "Off"), ("1", "On"), ("2", "Fullscreen only")],
                160.0,
                move |s| edit(CollectionAction::Monitor(i, MonitorEdit::Vrr(s))),
            ),
            field(
                "Mirror",
                extra_field(&m.extra, "mirror"),
                "DP-2",
                MonitorEdit::Mirror
            )
            .width(Length::Fixed(110.0)),
        ]
        .spacing(10),
    ]
    .spacing(8)
    .into()
}

// ---------------------------------------------------------------------------
// filtering
// ---------------------------------------------------------------------------

/// A plain-text rendition of an entry, for the filter box.
fn entry_text(loaded: &Loaded, id: CollectionId, i: usize) -> String {
    let c = &loaded.config;
    match id {
        CollectionId::Keybinds => {
            let k = &c.keybinds[i].value;
            format!(
                "{} {} {} {} {} {}",
                k.mods,
                k.key,
                k.dispatcher,
                k.args,
                k.description.as_deref().unwrap_or(""),
                k.submap.as_deref().unwrap_or("")
            )
        }
        CollectionId::WindowRules => {
            let r = &c.window_rules[i].value;
            format!("{} {}", r.rule, r.matchers)
        }
        CollectionId::LayerRules => {
            let r = &c.layer_rules[i].value;
            format!("{} {}", r.rule, r.namespace)
        }
        CollectionId::Workspaces => {
            let w = &c.workspaces[i].value;
            format!("{} {}", w.selector, w.rules)
        }
        CollectionId::Beziers => c.beziers[i].value.name.clone(),
        CollectionId::Animations => {
            let a = &c.animations[i].value;
            format!(
                "{} {} {}",
                a.name,
                a.curve,
                a.style.as_deref().unwrap_or("")
            )
        }
        CollectionId::Gestures => {
            let g = &c.gestures[i].value;
            format!("{} {} {} {}", g.direction, g.action, g.args, g.mods)
        }
        CollectionId::Devices => {
            let d = &c.devices[i].value;
            let opts: Vec<String> = d.options.iter().map(|(k, v)| format!("{k} {v}")).collect();
            format!("{} {}", d.name, opts.join(" "))
        }
        CollectionId::Monitors => monitor_line(&c.monitors[i].value),
        CollectionId::Env => {
            let e = &c.env[i].value;
            format!("{} {}", e.name, e.value)
        }
        CollectionId::Execs => c.execs[i].value.command.clone(),
        CollectionId::Variables => {
            let v = &c.variables[i].value;
            format!("{} {}", v.name, v.value)
        }
        CollectionId::Submaps => c.submaps[i].value.name.clone(),
        CollectionId::Permissions => {
            let p = &c.permissions[i].value;
            format!("{} {} {}", p.binary, p.kind, p.mode)
        }
        CollectionId::Plugins => c.plugins[i].value.path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_bodies_split_into_effect_and_value() {
        assert_eq!(split_rule("opacity 0.9 0.8"), ("opacity", "0.9 0.8"));
        assert_eq!(split_rule("float"), ("float", ""));
        assert_eq!(join_rule("size", " 800 600 "), "size 800 600");
        assert_eq!(join_rule("float", ""), "float");
    }

    #[test]
    fn every_dispatcher_is_offered_with_help() {
        assert!(DISPATCHERS.len() >= 60);
        assert!(dispatcher_help("workspace").unwrap().contains("args:"));
        assert!(DISPATCHERS.iter().all(|(_, help)| !help.is_empty()));
    }
}

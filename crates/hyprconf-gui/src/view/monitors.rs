// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Monitors screen.
//!
//! Unlike the other collections, this one is driven by **hardware, not text**:
//! it lists the displays actually attached (via `hyprctl monitors all -j`) and
//! treats the `monitor =` rules as the settings *behind* each display rather
//! than as the subject matter. A display the config never mentions still gets a
//! card; editing it authors a rule seeded from how it is currently running.
//!
//! Rules that don't name an attached connector — catch-alls, `desc:` selectors,
//! screens that are unplugged — are never hidden: they get their own section
//! with the raw field editor, because losing configuration silently would be
//! far worse than an unfamiliar layout.

use iced::widget::{button, canvas, column, container, pick_list, row, text, toggler, Space};
use iced::{Alignment, Element, Length, Padding};

use hyprconf_core::outputs::{logical_size, DetectedMonitor, Mode};
use hyprconf_core::schema::OptionSpec;
use hyprconf_core::structured::MonitorRule;
use hyprconf_core::{Tracked, Value};

use crate::edit::{extra_field, fmt_num, monitor_rule_targets, EditAction, MonitorEdit};
use crate::load::Loaded;
use crate::{App, Message};

use super::monitor_canvas::{MonitorLayout, Tile};
use super::styles::*;
use super::{coll_text, monitor_row, pane_header, scroll, EnumChoice, BOLD};

/// Transform values, in Hyprland's numbering.
const TRANSFORMS: &[(&str, &str)] = &[
    ("0", "Normal"),
    ("1", "90°"),
    ("2", "180°"),
    ("3", "270°"),
    ("4", "Flipped"),
    ("5", "Flipped + 90°"),
    ("6", "Flipped + 180°"),
    ("7", "Flipped + 270°"),
];

/// Scale factors offered in the dropdown. Any other value the user has written
/// stays selectable; these are just the ones worth one click.
const SCALES: &[(&str, &str)] = &[
    ("1", "100%"),
    ("1.25", "125%"),
    ("1.5", "150%"),
    ("1.75", "175%"),
    ("2", "200%"),
    ("auto", "Let Hyprland decide"),
];

/// Modes that aren't a resolution: let the compositor pick.
const MODE_PRESETS: &[(&str, &str)] = &[
    ("preferred", "The display’s preferred mode"),
    ("highres", "Highest resolution"),
    ("highrr", "Highest refresh rate"),
];

/// Compositor-wide display options, shown beneath the per-display cards because
/// they explain behaviour the cards can't (why a screen sleeps, why frames are
/// skipped).
const DISPLAY_SETTINGS: &[&str] = &[
    "misc:vrr",
    "debug:vfr",
    "misc:mouse_move_enables_dpms",
    "misc:key_press_enables_dpms",
];

// ---------------------------------------------------------------------------
// page
// ---------------------------------------------------------------------------

/// The whole Monitors pane.
pub(super) fn view(app: &App, loaded: &Loaded) -> Element<'static, Message> {
    let outputs = &app.outputs;
    let mut items: Vec<Element<Message>> = vec![header(app, loaded)];

    if outputs.is_empty() {
        items.push(no_outputs_notice(app));
    } else {
        items.push(layout_card(loaded, outputs));
        for (i, output) in outputs.iter().enumerate() {
            items.push(monitor_card(app, loaded, output, i + 1));
        }
    }

    items.extend(unmatched_rules(loaded, outputs));
    items.push(Space::new().height(Length::Fixed(6.0)).into());
    items.push(display_settings(app, loaded));

    scroll(items)
}

fn header(app: &App, loaded: &Loaded) -> Element<'static, Message> {
    let connected = app.outputs.len();
    let rules = loaded.config.monitors.len();
    let summary = if connected == 0 {
        format!("{rules} rule{}", plural(rules))
    } else {
        format!(
            "{connected} display{} · {rules} rule{}",
            plural(connected),
            plural(rules)
        )
    };

    let trailing = row![
        text(summary).size(13).style(muted),
        button(text("⟳").size(15))
            .padding([3, 9])
            .on_press(Message::RefreshMonitors)
            .style(ghost_button),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    pane_header(
        "🖥",
        "Monitors",
        "Resolution, position, scale and rotation for each display.",
        trailing.into(),
    )
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Shown instead of the cards when nothing could be detected.
fn no_outputs_notice(app: &App) -> Element<'static, Message> {
    let reason = if app.hyprland.is_none() {
        "Hyprland isn’t running, so the attached displays can’t be read. \
         Existing rules are still fully editable below."
    } else {
        "No displays were reported. Existing rules are still fully editable below."
    };

    let mut body = column![
        text("Nothing detected").size(15).font(BOLD),
        text(reason).size(13).style(muted),
    ]
    .spacing(4);

    if let Some(error) = &app.outputs_error {
        body = body.push(text(format!("hyprctl: {error}")).size(11).style(muted));
    }

    container(body)
        .padding([12, 16])
        .width(Length::Fill)
        .style(card_style)
        .into()
}

// ---------------------------------------------------------------------------
// layout canvas
// ---------------------------------------------------------------------------

fn layout_card(loaded: &Loaded, outputs: &[DetectedMonitor]) -> Element<'static, Message> {
    let tiles: Vec<Tile> = outputs
        .iter()
        .enumerate()
        .map(|(i, output)| tile(loaded, output, i + 1))
        .collect();

    let caption = if tiles.len() > 1 {
        "Drag displays to reposition · edges snap to neighbours"
    } else {
        "Your desktop, to scale"
    };

    container(
        column![
            container(
                canvas(MonitorLayout { tiles })
                    .width(Length::Fill)
                    .height(Length::Fixed(190.0)),
            )
            .padding(6)
            .width(Length::Fill)
            .style(inset_style),
            container(text(caption).size(11).style(muted))
                .width(Length::Fill)
                .center_x(Length::Fill),
        ]
        .spacing(8),
    )
    .padding([12, 14])
    .width(Length::Fill)
    .style(card_style)
    .into()
}

fn tile(loaded: &Loaded, output: &DetectedMonitor, number: usize) -> Tile {
    let rule = rule_for(loaded, &output.name);
    let state = Effective::of(output, rule);

    // A rule may say `auto` (or nothing at all); the compositor's actual
    // placement is then the only truthful thing to draw.
    let (x, y, auto) = match parse_position(&state.position) {
        Some((x, y)) => (x, y, false),
        None => (output.x as f32, output.y as f32, true),
    };

    let (width, height) = match Mode::parse(&state.mode) {
        Some(mode) => logical_size(
            mode.width,
            mode.height,
            state.scale.parse().unwrap_or(1.0),
            state.transform.parse().unwrap_or(0),
        ),
        // `preferred`/`highres` don't name a size: fall back to what the display
        // is actually doing right now.
        None => output.logical_size(),
    };

    Tile {
        connector: output.name.clone(),
        number,
        x,
        y,
        width,
        height,
        enabled: state.enabled,
        focused: output.focused,
        auto,
    }
}

/// Parse a `WxH` position, e.g. `1920x0` or `-1080x200`.
fn parse_position(position: &str) -> Option<(f32, f32)> {
    let text = position.trim();
    // `-1080x0` starts with a sign, so only split on an `x` that follows a
    // digit — never the leading one.
    let split = text
        .char_indices()
        .skip(1)
        .find(|(_, c)| *c == 'x' || *c == 'X')?
        .0;
    let x: f32 = text[..split].trim().parse().ok()?;
    let y: f32 = text[split + 1..].trim().parse().ok()?;
    Some((x, y))
}

// ---------------------------------------------------------------------------
// per-display card
// ---------------------------------------------------------------------------

/// The settings actually in force for a display: what the rule says, or what the
/// hardware is doing where the rule is silent.
struct Effective {
    enabled: bool,
    mode: String,
    position: String,
    scale: String,
    transform: String,
    mirror: String,
    bitdepth: String,
    vrr: String,
    /// The mode to restore when the display is switched back on.
    restore_mode: String,
}

impl Effective {
    fn of(output: &DetectedMonitor, rule: Option<&MonitorRule>) -> Self {
        let detected_mode = output.current_mode();
        match rule {
            Some(r) => {
                let disabled = r.is_disabled();
                Self {
                    enabled: !disabled,
                    // A disabled rule's mode field is the word `disable`, which
                    // is not a mode — show what it would come back as.
                    mode: if disabled {
                        detected_mode.clone()
                    } else {
                        r.mode.clone()
                    },
                    position: r.position.clone(),
                    scale: r.scale.clone(),
                    transform: extra_field(&r.extra, "transform"),
                    mirror: extra_field(&r.extra, "mirror"),
                    bitdepth: extra_field(&r.extra, "bitdepth"),
                    vrr: extra_field(&r.extra, "vrr"),
                    restore_mode: detected_mode,
                }
            }
            None => Self {
                enabled: !output.disabled,
                mode: detected_mode.clone(),
                position: output.current_position(),
                scale: fmt_num(output.scale),
                transform: output.transform.to_string(),
                mirror: output.mirror_of.clone().unwrap_or_default(),
                bitdepth: String::new(),
                vrr: String::new(),
                restore_mode: detected_mode,
            },
        }
    }
}

fn rule_for<'a>(loaded: &'a Loaded, connector: &str) -> Option<&'a MonitorRule> {
    crate::edit::monitor_rule_index(&loaded.config.monitors, connector)
        .and_then(|i| loaded.config.monitors.get(i))
        .map(|t| &t.value)
}

fn monitor_card(
    app: &App,
    loaded: &Loaded,
    output: &DetectedMonitor,
    number: usize,
) -> Element<'static, Message> {
    let rule = rule_for(loaded, &output.name);
    let state = Effective::of(output, rule);
    let connector = output.name.clone();
    let expanded = app.expanded_monitors.contains(&connector);

    let mut items = vec![card_header(output, number, rule.is_some(), &state)];

    if state.enabled {
        items.push(
            column![
                setting_row(
                    "Resolution",
                    "Resolution and refresh rate",
                    resolution_picker(&connector, output, &state),
                    true,
                ),
                setting_row(
                    "Scale",
                    "Display scaling factor",
                    choice(
                        &connector,
                        &state.scale,
                        SCALES,
                        200.0,
                        MonitorEdit::Scale,
                        "(inherit)",
                    ),
                    false,
                ),
                setting_row(
                    "Transform",
                    "Screen rotation",
                    choice(
                        &connector,
                        &state.transform,
                        TRANSFORMS,
                        200.0,
                        MonitorEdit::Transform,
                        "Normal",
                    ),
                    false,
                ),
            ]
            .spacing(4)
            .into(),
        );
        items.push(advanced(&connector, output, &state, expanded));
    } else {
        items.push(
            container(
                text("This display is switched off in your configuration.")
                    .size(12)
                    .style(muted),
            )
            .padding([6, 4])
            .into(),
        );
    }

    container(iced::widget::Column::with_children(items).spacing(10))
        .padding([12, 14])
        .width(Length::Fill)
        .style(card_style)
        .into()
}

fn card_header(
    output: &DetectedMonitor,
    number: usize,
    managed: bool,
    state: &Effective,
) -> Element<'static, Message> {
    let mut left = row![text(format!("{number}. {}", output.label()))
        .size(15)
        .font(BOLD)]
    .spacing(10)
    .align_y(Alignment::Center);

    let caps = output.capabilities();
    if !caps.is_empty() {
        left = left.push(text("Supports:").size(11).style(muted));
        for cap in caps {
            left = left.push(
                container(text(cap).size(10))
                    .padding([1, 6])
                    .style(cap_badge_style),
            );
        }
    }

    let mut right = row![].spacing(8).align_y(Alignment::Center);
    if managed {
        // Says where the values come from: this display has a `monitor =` line,
        // so what you see is your config, not the compositor's choice.
        right = right.push(
            container(text("Managed").size(10))
                .padding([1, 6])
                .style(soft_badge_style),
        );
    }
    right = right.push(text(output.name.clone()).size(12).style(muted));

    let connector = output.name.clone();
    let restore = state.restore_mode.clone();
    right = right.push(
        toggler(state.enabled)
            .on_toggle(move |on| {
                Message::MonitorEdit(connector.clone(), MonitorEdit::Enabled(on, restore.clone()))
            })
            .size(20),
    );

    row![left.width(Length::Fill), right]
        .align_y(Alignment::Center)
        .into()
}

/// The collapsible block of fields most people never touch.
fn advanced(
    connector: &str,
    output: &DetectedMonitor,
    state: &Effective,
    expanded: bool,
) -> Element<'static, Message> {
    let toggle = button(
        row![
            text("Advanced").size(13).width(Length::Fill),
            text(if expanded { "⌄" } else { "›" }).size(13),
        ]
        .align_y(Alignment::Center),
    )
    .padding([9, 12])
    .width(Length::Fill)
    .on_press(Message::ToggleMonitorAdvanced(connector.to_string()))
    .style(ghost_button);

    let mut col = column![container(toggle).style(inset_style)].spacing(4);
    if !expanded {
        return col.into();
    }

    col = col.push(setting_row(
        "Position",
        "Top-left corner in the layout, or `auto`",
        text_field(
            connector,
            &state.position,
            "0x0 / auto",
            200.0,
            MonitorEdit::Position,
        ),
        false,
    ));
    col = col.push(setting_row(
        "Variable refresh rate",
        "Adaptive sync for this display only",
        choice(
            connector,
            &state.vrr,
            &[("0", "Off"), ("1", "On"), ("2", "Fullscreen only")],
            200.0,
            MonitorEdit::Vrr,
            "(global setting)",
        ),
        false,
    ));
    col = col.push(setting_row(
        "Colour depth",
        "Bits per colour channel",
        choice(
            connector,
            &state.bitdepth,
            &[("8", "8-bit"), ("10", "10-bit")],
            200.0,
            MonitorEdit::Bitdepth,
            "(default)",
        ),
        false,
    ));
    col = col.push(setting_row(
        "Mirror",
        "Duplicate another display onto this one",
        text_field(connector, &state.mirror, "DP-2", 200.0, MonitorEdit::Mirror),
        false,
    ));

    if !output.description.trim().is_empty() {
        col = col.push(
            container(
                text(format!("EDID: {}", output.description))
                    .size(10)
                    .style(muted),
            )
            .padding([2, 6]),
        );
    }
    col.into()
}

// ---------------------------------------------------------------------------
// rows & controls
// ---------------------------------------------------------------------------

/// A labelled settings row: name and explanation on the left, control on the
/// right. The first row of a group carries an accent edge, which is what makes
/// a stack of rows read as one group rather than four loose bars.
fn setting_row(
    label: &str,
    description: &str,
    control: Element<'static, Message>,
    first: bool,
) -> Element<'static, Message> {
    let body = row![
        column![
            text(label.to_string()).size(13),
            text(description.to_string()).size(11).style(muted),
        ]
        .spacing(2)
        .width(Length::Fill),
        control,
    ]
    .spacing(12)
    .align_y(Alignment::Center);

    let inner = container(body)
        .padding([9, 12])
        .width(Length::Fill)
        .style(inset_style);

    if !first {
        return inner.into();
    }
    // A 3px accent backing peeking out on the left — height-matched by
    // construction, unlike a fixed-size bar.
    container(inner)
        .padding(Padding {
            left: 3.0,
            ..Padding::ZERO
        })
        .width(Length::Fill)
        .style(accent_edge_style)
        .into()
}

/// A dropdown bound to one monitor field.
///
/// Always offers an explicit "unset" entry (labelled for the field, e.g.
/// `Normal` for a transform) and keeps any value already in the config
/// selectable even if we don't enumerate it.
fn choice(
    connector: &str,
    current: &str,
    variants: &[(&str, &str)],
    width: f32,
    make: fn(String) -> MonitorEdit,
    unset_label: &str,
) -> Element<'static, Message> {
    let mut choices = vec![EnumChoice {
        value: String::new(),
        label: unset_label.to_string(),
    }];
    for (value, description) in variants {
        choices.push(EnumChoice {
            value: (*value).to_string(),
            label: (*description).to_string(),
        });
    }
    if !choices.iter().any(|c| c.value == current) {
        choices.push(EnumChoice {
            value: current.to_string(),
            label: current.to_string(),
        });
    }
    let selected = choices.iter().find(|c| c.value == current).cloned();
    let connector = connector.to_string();

    pick_list(choices, selected, move |c: EnumChoice| {
        Message::MonitorEdit(connector.clone(), make(c.value))
    })
    .padding([6, 10])
    .text_size(13)
    .width(Length::Fixed(width))
    .into()
}

/// The resolution dropdown: every mode the display advertises, plus the
/// "let Hyprland choose" presets, plus whatever is already configured.
fn resolution_picker(
    connector: &str,
    output: &DetectedMonitor,
    state: &Effective,
) -> Element<'static, Message> {
    let mut choices: Vec<EnumChoice> = output
        .modes
        .iter()
        .map(|m| EnumChoice {
            value: m.to_hyprland(),
            label: m.label(),
        })
        .collect();
    for (value, description) in MODE_PRESETS {
        choices.push(EnumChoice {
            value: (*value).to_string(),
            label: format!("{value}  ·  {description}"),
        });
    }
    if !choices.iter().any(|c| c.value == state.mode) {
        // A mode the display no longer advertises (or a hand-written one) must
        // stay visible — silently switching the user's resolution would be
        // unforgivable.
        let label = if state.mode.trim().is_empty() {
            "(unset)".to_string()
        } else {
            format!("{}  ·  not advertised", state.mode)
        };
        choices.insert(
            0,
            EnumChoice {
                value: state.mode.clone(),
                label,
            },
        );
    }

    let selected = choices.iter().find(|c| c.value == state.mode).cloned();
    let connector = connector.to_string();
    pick_list(choices, selected, move |c: EnumChoice| {
        Message::MonitorEdit(connector.clone(), MonitorEdit::Mode(c.value))
    })
    .padding([6, 10])
    .text_size(13)
    .width(Length::Fixed(240.0))
    .into()
}

fn text_field(
    connector: &str,
    value: &str,
    placeholder: &'static str,
    width: f32,
    make: fn(String) -> MonitorEdit,
) -> Element<'static, Message> {
    let connector = connector.to_string();
    coll_text(value, placeholder, Length::Fixed(width), move |s| {
        Message::MonitorEdit(connector.clone(), make(s))
    })
}

// ---------------------------------------------------------------------------
// rules with no attached display
// ---------------------------------------------------------------------------

/// Rules that don't name an attached connector, shown with the raw field editor.
///
/// These are the wildcards, the `desc:` selectors and the screens currently
/// unplugged. They still shape the desktop, so hiding them would be a lie.
fn unmatched_rules(loaded: &Loaded, outputs: &[DetectedMonitor]) -> Vec<Element<'static, Message>> {
    let count = loaded.config.monitors.len();
    let orphans: Vec<usize> = loaded
        .config
        .monitors
        .iter()
        .enumerate()
        .filter(|(_, t)| !is_matched(t, outputs))
        .map(|(i, _)| i)
        .collect();

    if orphans.is_empty() {
        return Vec::new();
    }

    let mut items: Vec<Element<Message>> = vec![section_label(
        "Other rules",
        "Wildcards, EDID selectors and displays that aren’t plugged in.",
    )];
    items.extend(
        orphans
            .into_iter()
            .map(|i| monitor_row(i, &loaded.config.monitors[i].value, count)),
    );
    items
}

fn is_matched(rule: &Tracked<MonitorRule>, outputs: &[DetectedMonitor]) -> bool {
    outputs
        .iter()
        .any(|o| monitor_rule_targets(&rule.value, &o.name))
}

// ---------------------------------------------------------------------------
// compositor-wide display settings
// ---------------------------------------------------------------------------

fn display_settings(app: &App, loaded: &Loaded) -> Element<'static, Message> {
    let mut rows: Vec<Element<Message>> = Vec::new();
    for (i, path) in DISPLAY_SETTINGS.iter().enumerate() {
        let Some(opt) = app.schema.option(path) else {
            continue;
        };
        rows.push(setting_row(
            &opt.label,
            &opt.description,
            option_control(opt, loaded),
            i == 0,
        ));
    }

    container(
        column![
            text("Display Settings").size(15).font(BOLD),
            iced::widget::Column::with_children(rows).spacing(4),
        ]
        .spacing(10),
    )
    .padding([12, 14])
    .width(Length::Fill)
    .style(card_style)
    .into()
}

/// The compact right-hand control for a scalar option (a toggle or a dropdown).
fn option_control(opt: &OptionSpec, loaded: &Loaded) -> Element<'static, Message> {
    let path = opt.path.clone();
    match loaded.value_for(opt) {
        Value::Bool(on) => toggler(on)
            .on_toggle(move |b| Message::Edit(EditAction::SetBool(path.clone(), b)))
            .size(20)
            .into(),
        current => {
            let current = match current {
                Value::Enum(name) | Value::String(name) => name,
                other => hyprconf_core::conf::value_to_conf(&other),
            };
            let mut choices: Vec<EnumChoice> = opt
                .enum_variants()
                .unwrap_or_default()
                .iter()
                .map(|v| EnumChoice {
                    value: v.name.clone(),
                    label: v.description.clone().unwrap_or_else(|| v.name.clone()),
                })
                .collect();
            if !choices.iter().any(|c| c.value == current) {
                choices.insert(
                    0,
                    EnumChoice {
                        value: current.clone(),
                        label: current.clone(),
                    },
                );
            }
            let selected = choices.iter().find(|c| c.value == current).cloned();
            pick_list(choices, selected, move |c: EnumChoice| {
                Message::Edit(EditAction::SetEnum(path.clone(), c.value))
            })
            .padding([6, 10])
            .text_size(13)
            .width(Length::Fixed(200.0))
            .into()
        }
    }
}

fn section_label(title: &str, subtitle: &str) -> Element<'static, Message> {
    column![
        text(title.to_string()).size(15).font(BOLD),
        text(subtitle.to_string()).size(12).style(muted),
    ]
    .spacing(2)
    .padding([10, 4])
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output() -> DetectedMonitor {
        hyprconf_core::outputs::parse_monitors(
            r#"[{"name":"DP-1","make":"Dell","model":"U2720Q",
                "width":3840,"height":2160,"refreshRate":59.997,
                "x":1920,"y":0,"scale":2.0,"transform":0,
                "currentFormat":"XRGB8888","mirrorOf":"none",
                "availableModes":["3840x2160@60.00Hz","1920x1080@60.00Hz"]}]"#,
        )
        .unwrap()
        .remove(0)
    }

    fn rule(mode: &str, position: &str, scale: &str, extra: &[&str]) -> MonitorRule {
        MonitorRule {
            name: "DP-1".into(),
            mode: mode.into(),
            position: position.into(),
            scale: scale.into(),
            extra: extra.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn an_unconfigured_display_shows_what_the_hardware_reports() {
        let o = output();
        let e = Effective::of(&o, None);
        assert!(e.enabled);
        assert_eq!(e.mode, "3840x2160@60.00");
        assert_eq!(e.position, "1920x0");
        assert_eq!(e.scale, "2");
        assert_eq!(e.transform, "0");
    }

    #[test]
    fn a_rule_overrides_the_hardware_state() {
        let o = output();
        let e = Effective::of(
            &o,
            Some(&rule("1920x1080@60.00", "0x0", "1", &["transform", "3"])),
        );
        assert_eq!(e.mode, "1920x1080@60.00");
        assert_eq!(e.position, "0x0");
        assert_eq!(e.transform, "3");
    }

    #[test]
    fn a_disabled_rule_still_offers_a_mode_to_come_back_to() {
        let o = output();
        let e = Effective::of(&o, Some(&rule("disable", "0x0", "1", &[])));
        assert!(!e.enabled);
        assert_eq!(
            e.restore_mode, "3840x2160@60.00",
            "re-enabling restores the display's current mode, not the word `disable`"
        );
        assert_eq!(
            e.mode, "3840x2160@60.00",
            "the picker never shows `disable`"
        );
    }

    #[test]
    fn positions_parse_including_negative_coordinates() {
        assert_eq!(parse_position("1920x0"), Some((1920.0, 0.0)));
        assert_eq!(parse_position(" 0 x 0 "), Some((0.0, 0.0)));
        assert_eq!(
            parse_position("-1080x200"),
            Some((-1080.0, 200.0)),
            "a leading minus must not be mistaken for a separator"
        );
        assert_eq!(parse_position("auto"), None);
        assert_eq!(parse_position("auto-right"), None);
        assert_eq!(parse_position(""), None);
    }
}

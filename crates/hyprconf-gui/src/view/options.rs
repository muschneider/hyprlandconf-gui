// SPDX-License-Identifier: MIT OR Apache-2.0
//! Settings sections: options grouped by sub-section into cards, one compact
//! row per option, each with an editor shaped by its type.
//!
//! Design notes:
//!
//! - **Grouping.** `decoration` alone holds forty-odd options; a flat list of
//!   cards made "where is the blur stuff?" a scrolling exercise. Options are
//!   now grouped by their sub-section (`blur`, `shadow`, `groupbar`, …) into
//!   one card each, with hairline dividers between rows.
//! - **Inline descriptions.** What an option does is the first thing you need
//!   and was hidden behind an `ⓘ` hover. It is now shown under the label.
//! - **Direct manipulation.** Small enums are segmented controls (one click,
//!   every choice visible), numbers get a slider *and* −/+ steppers, gradients
//!   get a live preview, and per-side gaps can be linked or split.

use std::borrow::Cow;

use iced::widget::{
    button, column, container, pick_list, row, rule, slider, text, toggler, tooltip, Column, Space,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Theme};

use hyprconf_core::conf::value_to_conf;
use hyprconf_core::schema::{EnumVariant, OptionSpec, Section, ValueType};
use hyprconf_core::value::{Color as HyprColor, CssGap, Gradient};
use hyprconf_core::Value;

use super::nav::section_icon;
use super::styles::*;
use super::{centered, empty_note, pane_header, scroll, BOLD, MONO};
use crate::edit::{fmt_num, EditAction, Slot};
use crate::load::Loaded;
use crate::{fuzzy, App, Message, OptionFilter, Selection};

// ---------------------------------------------------------------------------
// section pane
// ---------------------------------------------------------------------------

pub(super) fn section_view<'a>(app: &'a App, loaded: &'a Loaded, id: &str) -> Element<'a, Message> {
    let Some(section) = app.schema.section(id) else {
        return centered(text("Unknown section"));
    };

    let total = section.options.len();
    let set = section
        .options
        .iter()
        .filter(|o| loaded.config.get(&o.path).is_some())
        .count();
    let modified = section
        .options
        .iter()
        .filter(|o| loaded.is_dirty(&o.path))
        .count();

    let mut items: Vec<Element<Message>> = vec![
        pane_header(
            section_icon(id),
            &section.label,
            &section.description,
            Space::new().width(0).into(),
        ),
        filter_bar(app.option_filter, total, set, modified),
    ];

    let filter = app.option_filter;
    let mut shown = 0usize;
    for (group, options) in groups(section) {
        let rows: Vec<Element<Message>> = options
            .into_iter()
            .filter(|o| filter.keeps(loaded, &o.path))
            .map(|o| option_row(app, o, loaded, Some(group)))
            .collect();
        if rows.is_empty() {
            continue;
        }
        shown += rows.len();
        items.push(group_card(group_title(group), rows));
    }

    if shown == 0 {
        items.push(empty_note(match filter {
            OptionFilter::Modified => "Nothing changed in this section yet.",
            OptionFilter::Set => "This section is entirely at Hyprland's defaults.",
            OptionFilter::All => "This section has no options.",
        }));
    }

    scroll(items)
}

/// Split a section's options into sub-section groups, in order of first
/// appearance (the section's own options first).
fn groups(section: &Section) -> Vec<(&str, Vec<&OptionSpec>)> {
    let mut out: Vec<(&str, Vec<&OptionSpec>)> = vec![("", Vec::new())];
    for opt in &section.options {
        let key = group_key(&section.id, &opt.path);
        match out.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push(opt),
            None => out.push((key, vec![opt])),
        }
    }
    out.retain(|(_, list)| !list.is_empty());
    out
}

/// The sub-section an option belongs to: `decoration:blur:size` → `blur`,
/// `general:col.active_border` → `col` (colours get their own group),
/// `decoration:rounding` → `` (the section itself).
fn group_key<'a>(section: &str, path: &'a str) -> &'a str {
    let rest = path
        .strip_prefix(section)
        .and_then(|r| r.strip_prefix(':'))
        .unwrap_or(path);
    match rest.rsplit_once(':') {
        Some((group, _)) => group,
        None if rest.starts_with("col.") => "col",
        None => "",
    }
}

fn group_title(key: &str) -> String {
    match key {
        "" => String::new(),
        "col" => "Colors".into(),
        "groupbar" => "Group bar".into(),
        "tablettool" => "Tablet tool".into(),
        "touchdevice" => "Touch device".into(),
        "virtualkeyboard" => "Virtual keyboard".into(),
        "snap" => "Snapping".into(),
        other => other
            .split(':')
            .map(|s| capitalize(&s.replace('_', " ")).into_owned())
            .collect::<Vec<_>>()
            .join(" › "),
    }
}

fn capitalize(s: &str) -> Cow<'_, str> {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_lowercase() => Cow::Owned(c.to_uppercase().chain(chars).collect()),
        _ => Cow::Borrowed(s),
    }
}

/// The label to show inside a group: `Blur size` reads as just `Size` under a
/// "Blur" heading. Outside a group (search results) the full label is kept.
fn display_label<'a>(opt: &'a OptionSpec, group: Option<&str>) -> Cow<'a, str> {
    let label = opt.label.as_str();
    let Some(group) = group.filter(|g| !g.is_empty() && *g != "col") else {
        return Cow::Borrowed(label);
    };
    let last = group.rsplit(':').next().unwrap_or(group).replace('_', " ");
    let stripped = label
        .get(..last.len())
        .filter(|head| head.eq_ignore_ascii_case(&last))
        .and_then(|_| label.get(last.len()..))
        .and_then(|rest| rest.strip_prefix(' '))
        .filter(|rest| !rest.is_empty());
    match stripped {
        Some(rest) => Cow::Owned(capitalize(rest).into_owned()),
        None => Cow::Borrowed(label),
    }
}

/// A card holding a group's rows, with an optional heading strip.
pub(super) fn group_card<'a>(
    title: String,
    rows: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut body = Column::new().width(Length::Fill);
    if !title.is_empty() {
        body = body.push(
            container(text(title).size(12).font(BOLD).style(muted))
                .padding([7, 16])
                .width(Length::Fill)
                .style(group_head_style),
        );
    }
    for (i, r) in rows.into_iter().enumerate() {
        if i > 0 {
            body = body.push(divider());
        }
        body = body.push(r);
    }
    container(body).width(Length::Fill).style(card_style).into()
}

pub(super) fn divider() -> Element<'static, Message> {
    container(rule::horizontal(1).style(divider_style))
        .padding([0, 12])
        .into()
}

/// The all / set / modified chips above a section's options, each carrying its
/// own count so the user can see what a filter would reveal before clicking it.
fn filter_bar(
    active: OptionFilter,
    total: usize,
    set: usize,
    modified: usize,
) -> Element<'static, Message> {
    let count = |filter: OptionFilter| match filter {
        OptionFilter::All => total,
        OptionFilter::Set => set,
        OptionFilter::Modified => modified,
    };
    let last = OptionFilter::ALL.len() - 1;
    let segments = row(OptionFilter::ALL
        .into_iter()
        .enumerate()
        .map(|(i, filter)| {
            let selected = filter == active;
            button(text(format!("{} {}", filter.label(), count(filter))).size(12))
                .padding([4, 12])
                .on_press(Message::SetOptionFilter(filter))
                .style(move |theme: &Theme, status| {
                    segment_style(theme, status, selected, i == 0, i == last)
                })
                .into()
        }));

    container(
        row![text("Show").size(12).style(muted), segments]
            .spacing(10)
            .align_y(Alignment::Center),
    )
    .padding([2, 4])
    .into()
}

// ---------------------------------------------------------------------------
// one option row
// ---------------------------------------------------------------------------

/// One option: identity (label, description, path) on the left, its editor on
/// the right, and a reset button that is only live when it would do something.
///
/// `opt` is borrowed from the `'static` schema, so labels, descriptions and
/// paths render without allocating; only strings that travel inside a
/// `Message` are cloned.
pub(super) fn option_row<'a>(
    app: &'a App,
    opt: &'a OptionSpec,
    loaded: &'a Loaded,
    group: Option<&str>,
) -> Element<'a, Message> {
    let path = &opt.path;
    let dirty = loaded.is_dirty(path);

    let mut title = row![text(display_label(opt, group)).size(14)]
        .spacing(6)
        .align_y(Alignment::Center);
    if dirty {
        title = title.push(tooltip(
            text("●").size(9).style(accent),
            container(text("Unsaved change").size(12))
                .padding([4, 8])
                .style(tooltip_style),
            tooltip::Position::Top,
        ));
    }
    if let Some(since) = &opt.since {
        title = title.push(
            container(text(format!("since {since}")).size(10))
                .padding([0, 6])
                .style(soft_badge_style),
        );
    }

    let mut identity = column![title].spacing(2);
    if !opt.description.is_empty() {
        identity = identity.push(text(&opt.description).size(12).style(muted));
    }
    identity = identity.push(text(path).size(10).font(MONO).style(faint));
    if let Some(err) = loaded.first_error(path) {
        identity = identity.push(text(format!("✕ {err}")).size(11).style(danger));
    }

    let reset = reset_button(path.clone(), loaded.is_default(opt));
    let body: Element<Message> = if full_width(app, opt, loaded) {
        column![
            row![identity.width(Length::Fill), reset]
                .spacing(8)
                .align_y(Alignment::Start),
            type_editor(app, opt, loaded),
        ]
        .spacing(8)
        .into()
    } else {
        row![
            identity.width(Length::FillPortion(5)),
            container(type_editor(app, opt, loaded))
                .width(Length::FillPortion(4))
                .align_x(Alignment::End),
            reset,
        ]
        .spacing(14)
        .align_y(Alignment::Center)
        .into()
    };

    container(body).padding([10, 16]).width(Length::Fill).into()
}

/// Whether an editor is too big for the right-hand control column.
fn full_width(app: &App, opt: &OptionSpec, loaded: &Loaded) -> bool {
    match &opt.value_type {
        ValueType::Gradient => true,
        ValueType::CssGap => gap_is_split(app, opt, loaded),
        ValueType::String => uses_choice_chips(opt),
        _ => false,
    }
}

fn reset_button(path: String, is_default: bool) -> Element<'static, Message> {
    let b = button(text("↺").size(13))
        .padding([3, 8])
        .style(ghost_button);
    // Resetting an option that is already at its default is a no-op.
    let b = if is_default {
        b
    } else {
        b.on_press(Message::Edit(EditAction::Reset(path)))
    };
    tooltip(
        b,
        container(
            text(if is_default {
                "Already the default"
            } else {
                "Reset to the default"
            })
            .size(12),
        )
        .padding([6, 10])
        .style(tooltip_style),
        tooltip::Position::Left,
    )
    .into()
}

fn type_editor<'a>(app: &'a App, opt: &'a OptionSpec, loaded: &'a Loaded) -> Element<'a, Message> {
    match &opt.value_type {
        ValueType::Bool => bool_editor(opt, loaded),
        ValueType::Int => number_editor(opt, loaded, true),
        ValueType::Float => number_editor(opt, loaded, false),
        ValueType::String if !opt.suggestions.is_empty() => choice_editor(opt, loaded),
        ValueType::String => string_editor(opt, loaded),
        ValueType::Enum(variants) => enum_editor(opt, variants, loaded),
        ValueType::Color => color_editor(opt, loaded),
        ValueType::Gradient => gradient_editor(opt, loaded),
        ValueType::Vec2 => vec2_editor(opt, loaded),
        ValueType::CssGap => gap_editor(app, opt, loaded),
        _ => text("(not editable here)").size(13).style(muted).into(),
    }
}

// ---------------------------------------------------------------------------
// editors
// ---------------------------------------------------------------------------

fn bool_editor(opt: &OptionSpec, loaded: &Loaded) -> Element<'static, Message> {
    let on = matches!(loaded.value_for(opt), Value::Bool(true));
    let path = opt.path.clone();
    toggler(on)
        .on_toggle(move |b| Message::Edit(EditAction::SetBool(path.clone(), b)))
        .size(22)
        .into()
}

/// The hard bounds a value must stay within (for steppers).
fn hard_bounds(opt: &OptionSpec) -> (f64, f64) {
    let range = opt.range.as_ref();
    (
        range.and_then(|r| r.min).unwrap_or(f64::NEG_INFINITY),
        range.and_then(|r| r.max).unwrap_or(f64::INFINITY),
    )
}

/// A sensible increment: the schema's own step, else 1 for integers, else a
/// round fraction of the slider span.
fn step_for(opt: &OptionSpec, is_int: bool) -> f64 {
    if let Some(step) = opt.range.and_then(|r| r.step) {
        return step;
    }
    if is_int {
        return 1.0;
    }
    let span = opt.slider_span().map_or(1.0, |(lo, hi)| hi - lo);
    [0.01, 0.02, 0.05, 0.1, 0.25, 0.5, 1.0, 5.0, 10.0]
        .into_iter()
        .rev()
        .find(|s| *s <= span / 20.0)
        .unwrap_or(0.01)
}

/// Round away floating-point noise (`0.30000000000000004`).
fn tidy(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

fn number_editor(opt: &OptionSpec, loaded: &Loaded, is_int: bool) -> Element<'static, Message> {
    let current = match loaded.value_for(opt) {
        Value::Int(i) => i as f64,
        Value::Float(x) => x,
        _ => 0.0,
    };
    let path = opt.path.clone();
    let make = move |v: f64| {
        if is_int {
            Message::Edit(EditAction::SetIntSlider(path.clone(), v.round() as i64))
        } else {
            Message::Edit(EditAction::SetFloatSlider(path.clone(), tidy(v)))
        }
    };
    numeric_controls(
        opt,
        loaded,
        current,
        step_for(opt, is_int),
        Slot::Main,
        make,
    )
}

/// Slider (when the option has a span) + `−` [value] `+`.
fn numeric_controls(
    opt: &OptionSpec,
    loaded: &Loaded,
    current: f64,
    step: f64,
    slot: Slot,
    make: impl Fn(f64) -> Message + Clone + 'static,
) -> Element<'static, Message> {
    let path = &opt.path;
    let (lo, hi) = hard_bounds(opt);
    let draft = loaded
        .draft(path, slot.clone())
        .map(str::to_string)
        .unwrap_or_else(|| fmt_num(current));
    let has_err = loaded.field_error(path, slot.clone()).is_some();

    let down = (current - step).max(lo);
    let up = (current + step).min(hi);
    let minus = stepper("−", (down < current).then(|| make(down)));
    let plus = stepper("+", (up > current).then(|| make(up)));
    let input = super::text_field(&draft, path, slot, has_err, Length::Fixed(64.0), "");

    let mut controls = row![].spacing(10).align_y(Alignment::Center);
    if let Some((min, max)) = opt.slider_span() {
        controls = controls.push(
            slider(min..=max, current.clamp(min, max), make.clone())
                .step(step)
                .width(Length::Fill),
        );
    }
    controls
        .push(
            row![minus, input, plus]
                .spacing(2)
                .align_y(Alignment::Center),
        )
        .into()
}

fn stepper(glyph: &'static str, message: Option<Message>) -> Element<'static, Message> {
    let mut b = button(container(text(glyph).size(14)).center_x(Length::Fixed(14.0)))
        .padding([3, 6])
        .style(ghost_button);
    if let Some(m) = message {
        b = b.on_press(m);
    }
    b.into()
}

fn string_editor(opt: &OptionSpec, loaded: &Loaded) -> Element<'static, Message> {
    let path = opt.path.clone();
    let current = match loaded.value_for(opt) {
        Value::String(s) => s,
        other => value_to_conf(&other),
    };
    let draft = loaded
        .draft(&path, Slot::Main)
        .map(str::to_string)
        .unwrap_or(current);
    super::text_field(&draft, &path, Slot::Main, false, Length::Fill, "(unset)")
}

/// A selectable choice: the dropdown shows a human-friendly label while edits
/// round-trip the literal `value` written to the config.
///
/// Equality is by `value` only so the `pick_list` highlights the active choice
/// regardless of how its label is formatted.
#[derive(Clone)]
pub(crate) struct EnumChoice {
    pub(crate) value: String,
    pub(crate) label: String,
}

impl PartialEq for EnumChoice {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl std::fmt::Display for EnumChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

/// The leading clause of a description (`Disabled: …` → `Disabled`).
fn headline(description: &str) -> &str {
    description
        .split([':', '.', '('])
        .next()
        .unwrap_or(description)
        .trim()
}

/// A dropdown label: the literal plus what it means (`1 · Follow`).
fn choice_label(name: &str, description: Option<&str>) -> String {
    let shown = if name.is_empty() { "(unset)" } else { name };
    match description
        .map(headline)
        .filter(|h| !h.is_empty() && *h != name)
    {
        Some(h) => format!("{shown}  ·  {h}"),
        None => shown.to_string(),
    }
}

/// A segment's caption: numeric literals (`0`, `1`, `2`) mean nothing on
/// their own, so they show their meaning; words show themselves.
fn segment_label(v: &EnumVariant) -> String {
    let numeric = v.name.parse::<f64>().is_ok();
    if v.name.is_empty() {
        "Auto".into()
    } else if numeric {
        v.description
            .as_deref()
            .map(headline)
            .filter(|h| !h.is_empty())
            .map_or_else(|| v.name.clone(), str::to_string)
    } else {
        v.name.clone()
    }
}

/// A joined row of mutually exclusive choices: `(value, label, tooltip)`.
pub(super) fn segmented<'a>(
    items: Vec<(String, String, Option<String>)>,
    current: &str,
    on_select: impl Fn(String) -> Message + 'a,
) -> Element<'a, Message> {
    let last = items.len().saturating_sub(1);
    let mut segments = row![];
    for (i, (value, label, tip)) in items.into_iter().enumerate() {
        let selected = value == current;
        let b = button(text(label).size(12))
            .padding([5, 10])
            .on_press(on_select(value))
            .style(move |theme: &Theme, status| {
                segment_style(theme, status, selected, i == 0, i == last)
            });
        segments = segments.push(match tip {
            Some(tip) => Element::from(tooltip(
                b,
                container(text(tip).size(12))
                    .padding([6, 10])
                    .max_width(300.0)
                    .style(tooltip_style),
                tooltip::Position::Top,
            )),
            None => b.into(),
        });
    }
    segments.into()
}

fn enum_editor<'a>(
    opt: &'a OptionSpec,
    variants: &'a [EnumVariant],
    loaded: &Loaded,
) -> Element<'a, Message> {
    let path = opt.path.clone();
    let current = match loaded.value_for(opt) {
        Value::Enum(name) | Value::String(name) => name,
        other => value_to_conf(&other),
    };
    let known = variants.iter().any(|v| v.name == current);

    // Few, short choices: show them all; one click to change.
    let labels: Vec<String> = variants.iter().map(segment_label).collect();
    let fits = variants.len() <= 4 && labels.iter().all(|l| l.chars().count() <= 16) && known;
    if fits {
        let items = variants
            .iter()
            .zip(labels)
            .map(|(v, label)| (v.name.clone(), label, v.description.clone()))
            .collect();
        return segmented(items, &current, move |v| {
            Message::Edit(EditAction::SetEnum(path.clone(), v))
        });
    }

    let mut choices: Vec<EnumChoice> = variants
        .iter()
        .map(|v| EnumChoice {
            value: v.name.clone(),
            label: choice_label(&v.name, v.description.as_deref()),
        })
        .collect();
    // A value Hyprland accepts but the schema doesn't enumerate must stay
    // visible and selectable rather than vanish from the control.
    if !known {
        choices.insert(
            0,
            EnumChoice {
                value: current.clone(),
                label: if current.is_empty() {
                    "(unset)".into()
                } else {
                    current.clone()
                },
            },
        );
    }
    let selected = choices.iter().find(|c| c.value == current).cloned();
    pick_list(choices, selected, move |c: EnumChoice| {
        Message::Edit(EditAction::SetEnum(path.clone(), c.value))
    })
    .padding([6, 10])
    .text_size(13)
    .width(Length::Fill)
    .into()
}

/// Whether an open choice renders its suggestions as segments.
fn uses_choice_chips(opt: &OptionSpec) -> bool {
    (1..=4).contains(&opt.suggestions.len())
}

/// An *open* choice (`general:layout`, font weights): the suggestions as a
/// list, plus a field for anything else Hyprland accepts (`lua:my-layout`).
fn choice_editor<'a>(opt: &'a OptionSpec, loaded: &Loaded) -> Element<'a, Message> {
    let path = opt.path.clone();
    let current = match loaded.value_for(opt) {
        Value::String(s) | Value::Enum(s) => s,
        other => value_to_conf(&other),
    };
    // The custom field only holds a value when it *is* custom; echoing the
    // selected suggestion next to its own highlighted chip is just noise.
    let suggested = opt.suggestions.iter().any(|v| v.name == current);
    let draft = loaded
        .draft(&path, Slot::Main)
        .filter(|d| !opt.suggestions.iter().any(|v| v.name == *d))
        .map(str::to_string)
        .unwrap_or_else(|| {
            if suggested {
                String::new()
            } else {
                current.clone()
            }
        });
    let set = {
        let path = path.clone();
        move |v: String| Message::Edit(EditAction::EditText(path.clone(), Slot::Main, v))
    };

    let picker: Element<Message> = if uses_choice_chips(opt) {
        let items = opt
            .suggestions
            .iter()
            .map(|v| (v.name.clone(), v.name.clone(), v.description.clone()))
            .collect();
        segmented(items, &current, set)
    } else {
        let mut choices: Vec<EnumChoice> = opt
            .suggestions
            .iter()
            .map(|v| EnumChoice {
                value: v.name.clone(),
                label: choice_label(&v.name, v.description.as_deref()),
            })
            .collect();
        if !choices.iter().any(|c| c.value == current) {
            choices.insert(
                0,
                EnumChoice {
                    value: current.clone(),
                    label: format!("{current}  ·  custom"),
                },
            );
        }
        let selected = choices.iter().find(|c| c.value == current).cloned();
        pick_list(choices, selected, move |c: EnumChoice| set(c.value))
            .padding([6, 10])
            .text_size(13)
            .width(Length::Fill)
            .into()
    };

    let custom = super::text_field(
        &draft,
        &path,
        Slot::Main,
        false,
        Length::Fixed(130.0),
        "custom…",
    );
    let custom = tooltip(
        custom,
        container(text("Or type any value Hyprland accepts").size(12))
            .padding([6, 10])
            .style(tooltip_style),
        tooltip::Position::Top,
    );
    row![picker, custom]
        .spacing(10)
        .align_y(Alignment::Center)
        .into()
}

fn color_editor(opt: &OptionSpec, loaded: &Loaded) -> Element<'static, Message> {
    let path = opt.path.clone();
    let color = match loaded.value_for(opt) {
        Value::Color(c) => c,
        _ => HyprColor::rgba(0, 0, 0, 0xff),
    };
    let hex_draft = loaded
        .draft(&path, Slot::Hex)
        .map(str::to_string)
        .unwrap_or_else(|| color.to_rgba_string());
    let hex_err = loaded.field_error(&path, Slot::Hex).is_some();

    row![
        swatch_button(color, Message::OpenColorPicker(path.clone()), 40.0, 26.0),
        super::text_field(
            &hex_draft,
            &path,
            Slot::Hex,
            hex_err,
            Length::Fixed(150.0),
            "rgba(rrggbbaa)"
        ),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn gradient_editor(opt: &OptionSpec, loaded: &Loaded) -> Element<'static, Message> {
    let path = opt.path.clone();
    let gradient = match loaded.value_for(opt) {
        Value::Gradient(g) => g,
        _ => return text("(invalid gradient)").size(13).style(danger).into(),
    };
    let count = gradient.stops.len();

    let mut stops = row![].spacing(6).align_y(Alignment::Center);
    for (i, stop) in gradient.stops.iter().enumerate() {
        let swatch = swatch_button(
            *stop,
            Message::OpenStopColorPicker(path.clone(), i),
            34.0,
            24.0,
        );
        let remove = button(text("✕").size(10))
            .padding([2, 5])
            .style(ghost_button);
        let remove = if count > 1 {
            remove.on_press(Message::Edit(EditAction::RemoveStop(path.clone(), i)))
        } else {
            remove
        };
        stops = stops.push(row![swatch, remove].spacing(0).align_y(Alignment::Center));
    }
    let add = tooltip(
        button(text("+ stop").size(12))
            .padding([4, 10])
            .on_press(Message::Edit(EditAction::AddStop(path.clone())))
            .style(ghost_button),
        container(text("Add a colour stop (borders blend between stops)").size(12))
            .padding([6, 10])
            .style(tooltip_style),
        tooltip::Position::Top,
    );

    let angle = gradient.angle_deg.unwrap_or(0.0);
    let angle_draft = loaded
        .draft(&path, Slot::Angle)
        .map(str::to_string)
        .unwrap_or_else(|| gradient.angle_deg.map(fmt_num).unwrap_or_default());
    let angle_err = loaded.field_error(&path, Slot::Angle).is_some();
    let slider_path = path.clone();
    let angle_controls = row![
        text("Angle").size(12).style(muted),
        slider(0.0..=360.0, angle.clamp(0.0, 360.0), move |v| {
            Message::Edit(EditAction::EditText(
                slider_path.clone(),
                Slot::Angle,
                fmt_num(v.round()),
            ))
        })
        .step(1.0)
        .width(Length::Fixed(150.0)),
        super::text_field(
            &angle_draft,
            &path,
            Slot::Angle,
            angle_err,
            Length::Fixed(56.0),
            "0"
        ),
        text("°").size(12).style(muted),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    column![
        gradient_bar(&gradient, 22.0),
        row![stops, add, Space::new().width(Length::Fill), angle_controls]
            .spacing(8)
            .align_y(Alignment::Center),
    ]
    .spacing(8)
    .into()
}

/// A live preview of a gradient, drawn the way Hyprland blends it.
fn gradient_bar(gradient: &Gradient, height: f32) -> Element<'static, Message> {
    let colors: Vec<Color> = gradient.stops.iter().map(|c| iced_color(*c)).collect();
    // Hyprland's 0° runs left → right; iced's 0 rad points up.
    let angle = gradient.angle_deg.unwrap_or(0.0) as f32;
    container(
        Space::new()
            .width(Length::Fill)
            .height(Length::Fixed(height)),
    )
    .style(move |theme: &Theme| {
        let background = if colors.len() <= 1 {
            Background::Color(colors.first().copied().unwrap_or(Color::BLACK))
        } else {
            let last = (colors.len() - 1) as f32;
            let linear = colors.iter().enumerate().take(8).fold(
                iced::gradient::Linear::new(iced::Radians((angle + 90.0).to_radians())),
                |g, (i, c)| g.add_stop(i as f32 / last, *c),
            );
            Background::Gradient(iced::Gradient::Linear(linear))
        };
        container::Style {
            background: Some(background),
            border: Border {
                color: theme.extended_palette().background.strong.color,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        }
    })
    .into()
}

fn iced_color(c: HyprColor) -> Color {
    Color::from_rgba8(c.r, c.g, c.b, f32::from(c.a) / 255.0)
}

/// A color swatch that opens the visual picker.
fn swatch_button(color: HyprColor, message: Message, w: f32, h: f32) -> Element<'static, Message> {
    let fill = iced_color(color);
    let swatch = container(
        Space::new()
            .width(Length::Fixed(w))
            .height(Length::Fixed(h)),
    )
    .style(move |theme: &Theme| container::Style {
        background: Some(fill.into()),
        border: Border {
            color: theme.extended_palette().background.strong.color,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    });
    tooltip(
        button(swatch)
            .padding(2)
            .on_press(message)
            .style(ghost_button),
        container(text("Pick a color").size(12))
            .padding([6, 10])
            .style(tooltip_style),
        tooltip::Position::Top,
    )
    .into()
}

fn vec2_editor(opt: &OptionSpec, loaded: &Loaded) -> Element<'static, Message> {
    let path = opt.path.clone();
    let vec2 = match loaded.value_for(opt) {
        Value::Vec2(v) => v,
        _ => hyprconf_core::value::Vec2::new(0.0, 0.0),
    };
    let xd = loaded
        .draft(&path, Slot::X)
        .map(str::to_string)
        .unwrap_or_else(|| fmt_num(vec2.x));
    let yd = loaded
        .draft(&path, Slot::Y)
        .map(str::to_string)
        .unwrap_or_else(|| fmt_num(vec2.y));
    let xe = loaded.field_error(&path, Slot::X).is_some();
    let ye = loaded.field_error(&path, Slot::Y).is_some();

    row![
        text("x").size(12).style(muted),
        super::text_field(&xd, &path, Slot::X, xe, Length::Fixed(80.0), "0"),
        text("y").size(12).style(muted),
        super::text_field(&yd, &path, Slot::Y, ye, Length::Fixed(80.0), "0"),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn gap_is_split(app: &App, opt: &OptionSpec, loaded: &Loaded) -> bool {
    !loaded.current_gap(&opt.path, opt).is_uniform() || app.split_gaps.contains(&opt.path)
}

/// Per-side gaps: one linked value (slider + steppers) by default, or four
/// fields — top, right, bottom, left — once split.
fn gap_editor<'a>(app: &'a App, opt: &'a OptionSpec, loaded: &'a Loaded) -> Element<'a, Message> {
    let path = opt.path.clone();
    let gap = loaded.current_gap(&path, opt);

    if gap_is_split(app, opt, loaded) {
        let mut sides = row![].spacing(10).align_y(Alignment::End);
        for (i, name) in ["Top", "Right", "Bottom", "Left"].into_iter().enumerate() {
            let draft = loaded
                .draft(&path, Slot::Side(i))
                .map(str::to_string)
                .unwrap_or_else(|| gap.sides()[i].to_string());
            let err = loaded.field_error(&path, Slot::Side(i)).is_some();
            sides = sides.push(
                column![
                    text(name).size(10).style(muted),
                    super::text_field(&draft, &path, Slot::Side(i), err, Length::Fixed(64.0), "0"),
                ]
                .spacing(2),
            );
        }
        return row![
            sides,
            Space::new().width(Length::Fill),
            link_button("⛓ link sides", "Use one value for every side", path),
        ]
        .spacing(10)
        .align_y(Alignment::End)
        .into();
    }

    let make = {
        let path = path.clone();
        move |v: f64| {
            Message::Edit(EditAction::SetGap(
                path.clone(),
                CssGap::uniform(v.round().max(0.0) as i64),
            ))
        }
    };
    row![
        numeric_controls(opt, loaded, gap.top as f64, 1.0, Slot::Main, make),
        link_button("⊞", "Set each side separately", path),
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .into()
}

fn link_button(label: &'static str, tip: &'static str, path: String) -> Element<'static, Message> {
    tooltip(
        button(text(label).size(12))
            .padding([4, 8])
            .on_press(Message::ToggleGapSides(path))
            .style(ghost_button),
        container(text(tip).size(12))
            .padding([6, 10])
            .style(tooltip_style),
        tooltip::Position::Top,
    )
    .into()
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

/// The search pane: results are *live editors*, not links — you type, you see
/// the control, you change it. The section link is there for context.
pub(super) fn search_results<'a>(app: &'a App, loaded: &'a Loaded) -> Element<'a, Message> {
    let query = app.search.trim();
    let hits = &app.hits;

    if hits.is_empty() {
        return centered(
            column![
                text("No matches").size(18).font(BOLD),
                text(format!("Nothing in the schema matches “{query}”."))
                    .size(13)
                    .style(muted),
                text("Try a shorter word, or part of the option path (e.g. “blur”).")
                    .size(12)
                    .style(muted),
            ]
            .spacing(6)
            .align_x(Alignment::Center),
        );
    }

    let summary = if hits.hidden() > 0 {
        format!(
            "{} of {} matches — keep typing to narrow it down",
            hits.options.len(),
            hits.total
        )
    } else {
        format!(
            "{} match{}",
            hits.total,
            if hits.total == 1 { "" } else { "es" }
        )
    };

    let mut items: Vec<Element<Message>> = vec![container(
        row![
            text(summary).size(12).style(muted),
            Space::new().width(Length::Fill),
            text("Esc to clear").size(11).style(muted),
        ]
        .align_y(Alignment::Center),
    )
    .padding([2, 6])
    .into()];

    // Matching collections first: they're whole screens, the coarser hit.
    for id in &hits.collections {
        let label = app
            .schema
            .collection(*id)
            .map(|c| c.label.as_str())
            .unwrap_or_default();
        items.push(
            button(
                row![
                    text(super::nav::collection_icon(*id)).size(14),
                    text(label).size(14),
                    Space::new().width(Length::Fill),
                    text(format!(
                        "{} entries →",
                        super::nav::collection_count(app, *id)
                    ))
                    .size(12)
                    .style(muted),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([9, 14])
            .on_press(Message::Selected(Selection::Collection(*id)))
            .style(result_style)
            .into(),
        );
    }

    for hit in &hits.options {
        items.push(search_hit(app, hit, loaded));
    }

    scroll(items)
}

/// One search hit: a breadcrumb to its section, then the option's own row.
fn search_hit<'a>(
    app: &'a App,
    hit: &fuzzy::OptionHit,
    loaded: &'a Loaded,
) -> Element<'a, Message> {
    let section = hit.section;
    let label = app
        .schema
        .section(section)
        .map_or(section, |s| s.label.as_str());
    let group = group_title(group_key(section, &hit.spec.path));
    let crumb = if group.is_empty() {
        label.to_string()
    } else {
        format!("{label} › {group}")
    };
    let jump = button(
        row![
            text(section_icon(section)).size(11),
            text(crumb).size(11).style(muted),
        ]
        .spacing(5)
        .align_y(Alignment::Center),
    )
    .padding([1, 6])
    .on_press(Message::Selected(Selection::Section(section.to_string())))
    .style(ghost_button);

    column![
        container(jump).padding([0, 6]),
        container(option_row(app, hit.spec, loaded, None))
            .width(Length::Fill)
            .style(card_style),
    ]
    .spacing(2)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprconf_core::schema::Schema;

    #[test]
    fn options_group_by_sub_section() {
        assert_eq!(group_key("decoration", "decoration:blur:size"), "blur");
        assert_eq!(group_key("decoration", "decoration:rounding"), "");
        assert_eq!(group_key("general", "general:col.active_border"), "col");
        assert_eq!(group_key("group", "group:groupbar:col.active"), "groupbar");
        assert_eq!(group_title("motion_blur"), "Motion blur");
        assert_eq!(group_title("groupbar"), "Group bar");
    }

    #[test]
    fn labels_drop_the_group_prefix_inside_their_group() {
        let schema = Schema::shared();
        let label = |path: &str, group: &str| {
            display_label(schema.option(path).unwrap(), Some(group)).into_owned()
        };
        assert_eq!(label("decoration:blur:size", "blur"), "Size");
        assert_eq!(label("group:groupbar:font_size", "groupbar"), "Font size");
        assert_eq!(
            label("decoration:motion_blur:samples", "motion_blur"),
            "Samples"
        );
        // No redundant prefix: untouched.
        assert_eq!(
            label("input:touchpad:tap_to_click", "touchpad"),
            "Tap to click"
        );
        // Outside a group (search results), the full label stays.
        assert_eq!(
            display_label(schema.option("decoration:blur:size").unwrap(), None),
            "Blur size"
        );
    }

    #[test]
    fn every_section_groups_cover_all_options() {
        for section in Schema::shared().sections() {
            let grouped: usize = groups(section).iter().map(|(_, o)| o.len()).sum();
            assert_eq!(grouped, section.options.len(), "{}", section.id);
        }
    }

    #[test]
    fn numeric_segments_show_their_meaning() {
        let schema = Schema::shared();
        let labels: Vec<String> = schema
            .option("input:follow_mouse")
            .unwrap()
            .enum_variants()
            .unwrap()
            .iter()
            .map(segment_label)
            .collect();
        assert_eq!(labels, ["Disabled", "Follow", "Detached", "Separate"]);
    }

    #[test]
    fn float_steps_are_round_fractions_of_the_span() {
        let schema = Schema::shared();
        let step = |p: &str| step_for(schema.option(p).unwrap(), false);
        assert_eq!(step("decoration:active_opacity"), 0.05);
        assert_eq!(step("input:sensitivity"), 0.1);
        assert_eq!(tidy(0.1 + 0.2), 0.3);
    }
}

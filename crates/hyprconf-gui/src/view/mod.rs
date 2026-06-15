// SPDX-License-Identifier: MIT OR Apache-2.0
//! All rendering. Pure functions over `&App` producing Iced elements.

use iced::widget::{
    button, canvas, center, column, container, mouse_area, opaque, pick_list, row, scrollable,
    slider, stack, text, text_input, toggler, tooltip, Column, Space,
};
use iced::{Alignment, Border, Color, Element, Font, Length, Theme};

use hyprconf_core::conf::value_to_conf;
use hyprconf_core::schema::{CollectionId, OptionSpec, ValueType};
use hyprconf_core::structured::{
    Animation, Bezier, EnvVar, Exec, ExecKind, Keybind, LayerRule, MonitorRule, Submap, Variable,
    WindowRule, WorkspaceRule,
};
use hyprconf_core::value::Color as HyprColor;
use hyprconf_core::{ConfigFormat, Severity, Value};

use crate::diff::Tag;
use crate::edit::{
    env_issue, exec_issue, extra_field, fmt_num, has_mod, keybind_issue, layer_rule_issue,
    monitor_issue, parse_matchers, window_rule_issue, BindFlag, CollectionAction, ColorChannel,
    Dir, EditAction, EnvEdit, ExecEdit, KeybindEdit, LayerRuleEdit, MonitorEdit, Slot,
    WindowRuleEdit, DISPATCHERS, MODS,
};
use crate::load::{format_label, LoadState, Loaded};
use crate::save::{self, SaveMode};
use crate::{color_picker, fuzzy, App, Message, Selection};

mod styles;
use styles::*;

const SIDEBAR_WIDTH: f32 = 260.0;
const BOLD: Font = Font {
    weight: iced::font::Weight::Bold,
    ..Font::DEFAULT
};
const MONO: Font = Font::MONOSPACE;

/// The whole window: header / [sidebar | content] / status bar, with the color
/// picker floating above as a modal when open.
pub fn view(app: &App) -> Element<'_, Message> {
    let base: Element<Message> = column![
        header(app),
        row![sidebar(app), content(app)].height(Length::Fill),
        status_bar(app),
    ]
    .into();

    match &app.color_picker {
        Some(draft) => modal(
            base,
            color_picker_panel(app, draft),
            Message::CloseColorPicker,
        ),
        None => base,
    }
}

/// Float `content` (a dimmed, click-to-dismiss modal) above `base`.
fn modal<'a>(
    base: Element<'a, Message>,
    content: Element<'a, Message>,
    on_blur: Message,
) -> Element<'a, Message> {
    stack![
        base,
        opaque(
            mouse_area(center(opaque(content)).style(|_theme| {
                container::Style {
                    background: Some(
                        Color {
                            a: 0.7,
                            ..Color::BLACK
                        }
                        .into(),
                    ),
                    ..container::Style::default()
                }
            }))
            .on_press(on_blur)
        ),
    ]
    .into()
}

// ---------------------------------------------------------------------------
// header
// ---------------------------------------------------------------------------

fn header(app: &App) -> Element<'_, Message> {
    let brand = row![
        text("❖").size(22).style(accent),
        text("hyprconf").size(20).font(BOLD),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let search = text_input("Search options…", &app.search)
        .on_input(Message::SearchChanged)
        .padding([8, 12])
        .size(15)
        .width(Length::Fill);

    let theme_picker = row![
        text("Theme").size(13).style(muted),
        pick_list(Theme::ALL, Some(app.theme.clone()), Message::ThemeSelected)
            .text_size(13)
            .padding([6, 10])
            .width(Length::Fixed(170.0)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let mut bar = row![brand, search].spacing(20).align_y(Alignment::Center);
    if let Some(changes) = changes_indicator(app) {
        bar = bar.push(changes);
    }
    if app.load.loaded().is_some() {
        bar = bar.push(history_button(
            "↶",
            "Undo (Ctrl+Z)",
            (!app.undo.is_empty()).then_some(Message::Undo),
        ));
        bar = bar.push(history_button(
            "↷",
            "Redo (Ctrl+Shift+Z)",
            (!app.redo.is_empty()).then_some(Message::Redo),
        ));
        bar = bar.push(toolbar_toggle(
            "profiles",
            app.show_profiles,
            Message::ToggleProfiles,
        ));
        bar = bar.push(toolbar_toggle("save…", app.show_save, Message::ToggleSave));
    }
    bar = bar.push(theme_picker);

    container(bar)
        .padding([12, 18])
        .width(Length::Fill)
        .style(bar_style)
        .into()
}

/// The clickable "N unsaved" pill (only when a config is loaded).
fn changes_indicator(app: &App) -> Option<Element<'_, Message>> {
    let loaded = app.load.loaded()?;
    let count = loaded.total_unsaved();
    let (label, kind) = if count == 0 {
        ("no changes".to_string(), false)
    } else {
        (format!("● {count} unsaved"), true)
    };
    let style: fn(&Theme, button::Status) -> button::Style = if kind {
        changes_button_style
    } else {
        ghost_button
    };
    Some(
        button(text(label).size(13))
            .padding([6, 12])
            .on_press(Message::ToggleChanges)
            .style(style)
            .into(),
    )
}

/// A small icon button with a tooltip; disabled when `message` is `None`.
fn history_button(
    glyph: &'static str,
    tip: &'static str,
    message: Option<Message>,
) -> Element<'static, Message> {
    let mut b = button(text(glyph).size(15))
        .padding([6, 11])
        .style(ghost_button);
    if let Some(m) = message {
        b = b.on_press(m);
    }
    tooltip(
        b,
        container(text(tip).size(12))
            .padding([6, 10])
            .style(tooltip_style),
        tooltip::Position::Bottom,
    )
    .into()
}

/// A header toggle button (accent-tinted while its panel is open).
fn toolbar_toggle(
    label: &'static str,
    active: bool,
    message: Message,
) -> Element<'static, Message> {
    let style: fn(&Theme, button::Status) -> button::Style = if active {
        changes_button_style
    } else {
        ghost_button
    };
    button(text(label).size(13))
        .padding([6, 12])
        .on_press(message)
        .style(style)
        .into()
}

// ---------------------------------------------------------------------------
// sidebar
// ---------------------------------------------------------------------------

fn sidebar(app: &App) -> Element<'_, Message> {
    let compact = is_compact(app);
    let active = app.search.trim().is_empty() && !app.show_profiles;
    let mut items: Vec<Element<Message>> = Vec::new();

    if !compact {
        items.push(group_header("SECTIONS"));
    }
    for section in app.schema.sections() {
        let selection = Selection::Section(section.id.clone());
        let selected = active && app.selected == selection;
        items.push(nav_button(
            section_icon(&section.id),
            section.label.clone(),
            selection,
            selected,
            None,
            compact,
        ));
    }

    items.push(Space::new().height(Length::Fixed(14.0)).into());
    if !compact {
        items.push(group_header("COLLECTIONS"));
    }
    for collection in app.schema.collections() {
        let count = collection_count(app, collection.id);
        let selection = Selection::Collection(collection.id);
        let selected = active && app.selected == selection;
        items.push(nav_button(
            collection_icon(collection.id),
            collection.label.clone(),
            selection,
            selected,
            (!compact).then_some(count),
            compact,
        ));
    }

    let list = Column::with_children(items)
        .spacing(3)
        .padding([12, if compact { 6 } else { 10 }])
        .width(Length::Fill);

    container(scrollable(list).height(Length::Fill))
        .width(Length::Fixed(if compact { 60.0 } else { SIDEBAR_WIDTH }))
        .height(Length::Fill)
        .style(panel_style)
        .into()
}

/// Below this window width the sidebar collapses to icons-only.
fn is_compact(app: &App) -> bool {
    app.settings.window_width < 860.0
}

fn group_header(label: &str) -> Element<'_, Message> {
    container(text(label.to_string()).size(11).font(BOLD).style(muted))
        .padding([8, 8])
        .into()
}

fn nav_button(
    icon: &'static str,
    label: String,
    selection: Selection,
    selected: bool,
    badge: Option<usize>,
    compact: bool,
) -> Element<'static, Message> {
    if compact {
        let b = button(container(text(icon).size(16)).center_x(Length::Fill))
            .width(Length::Fill)
            .padding([8, 0])
            .on_press(Message::Selected(selection))
            .style(move |theme: &Theme, status| nav_style(theme, status, selected));
        return tooltip(
            b,
            container(text(label).size(12))
                .padding([6, 10])
                .style(tooltip_style),
            tooltip::Position::Right,
        )
        .into();
    }

    let mut inner = row![text(icon).size(15), text(label).size(14)]
        .spacing(10)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    if let Some(count) = badge {
        inner = inner.push(count_badge(count));
    }

    button(inner)
        .width(Length::Fill)
        .padding([7, 12])
        .on_press(Message::Selected(selection))
        .style(move |theme: &Theme, status| nav_style(theme, status, selected))
        .into()
}

fn count_badge(count: usize) -> Element<'static, Message> {
    container(text(count.to_string()).size(11))
        .padding([1, 7])
        .style(badge_style)
        .into()
}

fn collection_count(app: &App, id: CollectionId) -> usize {
    let Some(loaded) = app.load.loaded() else {
        return 0;
    };
    let c = &loaded.config;
    match id {
        CollectionId::Monitors => c.monitors.len(),
        CollectionId::Workspaces => c.workspaces.len(),
        CollectionId::WindowRules => c.window_rules.len(),
        CollectionId::LayerRules => c.layer_rules.len(),
        CollectionId::Keybinds => c.keybinds.len(),
        CollectionId::Submaps => c.submaps.len(),
        CollectionId::Env => c.env.len(),
        CollectionId::Execs => c.execs.len(),
        CollectionId::Variables => c.variables.len(),
        CollectionId::Beziers => c.beziers.len(),
        CollectionId::Animations => c.animations.len(),
    }
}

// ---------------------------------------------------------------------------
// content area
// ---------------------------------------------------------------------------

fn content(app: &App) -> Element<'_, Message> {
    if app.show_profiles {
        return container(profiles_view(app))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(20)
            .into();
    }
    let inner: Element<Message> = match &app.load {
        LoadState::Loading => centered(text("Loading configuration…").size(16).style(muted)),
        LoadState::NotFound { searched } => not_found_view(searched),
        LoadState::Error { path, message } => error_view(&path.display().to_string(), message),
        LoadState::Loaded(loaded) => {
            if app.show_save {
                // The preview is precomputed in `update`; `None` only if a
                // refresh was somehow missed, so degrade gracefully.
                match &app.save_preview {
                    Some(preview) => save_view(app, loaded, preview),
                    None => centered(text("Preparing save preview…").size(16).style(muted)),
                }
            } else if app.show_changes {
                changes_view(app, loaded)
            } else if app.search.trim().is_empty() {
                match &app.selected {
                    Selection::Section(id) => section_view(app, loaded, id),
                    Selection::Collection(id) => collection_view(app, loaded, *id),
                }
            } else {
                search_results(app, loaded)
            }
        }
    };

    container(inner)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(20)
        .into()
}

fn centered(content: impl Into<Element<'static, Message>>) -> Element<'static, Message> {
    container(content.into()).center(Length::Fill).into()
}

fn not_found_view(searched: &[std::path::PathBuf]) -> Element<'static, Message> {
    let mut lines: Vec<Element<Message>> = vec![
        text("🔍").size(40).into(),
        text("No Hyprland configuration found")
            .size(20)
            .font(BOLD)
            .into(),
        text("Looked for:").size(13).style(muted).into(),
    ];
    for path in searched {
        lines.push(text(format!("• {}", path.display())).size(13).into());
    }
    lines.push(Space::new().height(Length::Fixed(8.0)).into());
    lines.push(
        text("Pass --config <path> to load a specific file.")
            .size(13)
            .style(muted)
            .into(),
    );
    centered(
        Column::with_children(lines)
            .spacing(8)
            .align_x(Alignment::Center),
    )
}

fn error_view(path: &str, message: &str) -> Element<'static, Message> {
    centered(
        Column::with_children(vec![
            text("⚠").size(40).style(danger).into(),
            text("Failed to load configuration")
                .size(20)
                .font(BOLD)
                .into(),
            text(path.to_string()).size(13).style(muted).into(),
            text(message.to_string()).size(13).style(danger).into(),
        ])
        .spacing(8)
        .align_x(Alignment::Center),
    )
}

fn pane_header(
    icon: &str,
    title: String,
    subtitle: String,
    trailing: String,
) -> Element<'static, Message> {
    let mut left = row![text(icon.to_string()).size(26)]
        .spacing(12)
        .align_y(Alignment::Center);
    left = left.push(
        column![
            text(title).size(22).font(BOLD),
            text(subtitle).size(13).style(muted),
        ]
        .spacing(2),
    );

    container(
        row![
            left.width(Length::Fill),
            text(trailing).size(13).style(muted),
        ]
        .align_y(Alignment::Center),
    )
    .padding([14, 18])
    .width(Length::Fill)
    .style(card_style)
    .into()
}

fn section_view(app: &App, loaded: &Loaded, id: &str) -> Element<'static, Message> {
    let Some(section) = app.schema.section(id) else {
        return centered(text("Unknown section"));
    };

    let set = section
        .options
        .iter()
        .filter(|o| loaded.config.get(&o.path).is_some())
        .count();
    let trailing = format!("{set} set · {} total", section.options.len());

    let mut items: Vec<Element<Message>> = vec![
        pane_header(
            section_icon(id),
            section.label.clone(),
            section.description.clone(),
            trailing,
        ),
        Space::new().height(Length::Fixed(4.0)).into(),
    ];
    for opt in &section.options {
        items.push(option_editor(opt, loaded));
    }

    scroll(items)
}

// ---------------------------------------------------------------------------
// per-option editor
// ---------------------------------------------------------------------------

fn option_editor(opt: &OptionSpec, loaded: &Loaded) -> Element<'static, Message> {
    let path = opt.path.clone();
    let dirty = loaded.is_dirty(&path);
    let is_default = loaded.config.get(&path).is_none();

    let label_row = row![
        text(opt.label.clone()).size(15),
        info_tooltip(&opt.description),
        dirty_dot(dirty),
        Space::new().width(Length::Fill),
        reset_button(path.clone(), is_default),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let mut col = column![label_row, type_editor(opt, loaded)].spacing(10);

    if let Some(err) = loaded.first_error(&path) {
        col = col.push(text(format!("⚠ {err}")).size(11).style(danger));
    }
    col = col.push(text(opt.path.clone()).size(10).style(muted));

    container(col)
        .padding([12, 16])
        .width(Length::Fill)
        .style(card_style)
        .into()
}

fn info_tooltip(description: &str) -> Element<'static, Message> {
    if description.is_empty() {
        return Space::new().width(0).into();
    }
    tooltip(
        text("ⓘ").size(12).style(muted),
        container(text(description.to_string()).size(12))
            .padding([6, 10])
            .max_width(320.0)
            .style(tooltip_style),
        tooltip::Position::Bottom,
    )
    .into()
}

fn dirty_dot(dirty: bool) -> Element<'static, Message> {
    if dirty {
        text("●").size(10).style(accent).into()
    } else {
        Space::new().width(0).into()
    }
}

fn reset_button(path: String, is_default: bool) -> Element<'static, Message> {
    let button = button(text("↺ reset").size(12))
        .padding([3, 8])
        .style(ghost_button);
    // Resetting an already-default option is a no-op; disable the press then.
    let button = if is_default {
        button
    } else {
        button.on_press(Message::Edit(EditAction::Reset(path)))
    };
    tooltip(
        button,
        container(text("Reset to default").size(12))
            .padding([6, 10])
            .style(tooltip_style),
        tooltip::Position::Left,
    )
    .into()
}

fn type_editor(opt: &OptionSpec, loaded: &Loaded) -> Element<'static, Message> {
    match &opt.value_type {
        ValueType::Bool => bool_editor(opt, loaded),
        ValueType::Int => number_editor(opt, loaded, true),
        ValueType::Float => number_editor(opt, loaded, false),
        ValueType::String => string_editor(opt, loaded),
        ValueType::Enum(_) => enum_editor(opt, loaded),
        ValueType::Color => color_editor(opt, loaded),
        ValueType::Gradient => gradient_editor(opt, loaded),
        ValueType::Vec2 => vec2_editor(opt, loaded),
        _ => text("(not editable here)").size(13).style(muted).into(),
    }
}

fn bool_editor(opt: &OptionSpec, loaded: &Loaded) -> Element<'static, Message> {
    let on = matches!(loaded.value_for(opt), Value::Bool(true));
    let path = opt.path.clone();
    row![
        toggler(on)
            .on_toggle(move |b| Message::Edit(EditAction::SetBool(path.clone(), b)))
            .size(22),
        text(if on { "Enabled" } else { "Disabled" })
            .size(13)
            .style(muted),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

fn number_editor(opt: &OptionSpec, loaded: &Loaded, is_int: bool) -> Element<'static, Message> {
    let path = opt.path.clone();
    let current = match loaded.value_for(opt) {
        Value::Int(i) => i as f64,
        Value::Float(x) => x,
        _ => 0.0,
    };
    let draft = loaded
        .draft(&path, Slot::Main)
        .map(str::to_string)
        .unwrap_or_else(|| fmt_num(current));
    let has_err = loaded.field_error(&path, Slot::Main).is_some();

    let input = text_field(&draft, &path, Slot::Main, has_err, Length::Fixed(120.0), "");

    let mut row = row![]
        .spacing(14)
        .align_y(Alignment::Center)
        .width(Length::Fill);
    if let Some(range) = &opt.range {
        if let (Some(min), Some(max)) = (range.min, range.max) {
            let value = current.clamp(min, max);
            let p = path.clone();
            let step = if is_int {
                range.step.unwrap_or(1.0).max(1.0)
            } else {
                range.step.unwrap_or(((max - min) / 100.0).max(0.001))
            };
            let s = slider(min..=max, value, move |v| {
                if is_int {
                    Message::Edit(EditAction::SetIntSlider(p.clone(), v.round() as i64))
                } else {
                    Message::Edit(EditAction::SetFloatSlider(p.clone(), v))
                }
            })
            .step(step)
            .width(Length::Fill);
            row = row.push(s);
        }
    }
    row.push(input).into()
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
    text_field(&draft, &path, Slot::Main, false, Length::Fill, "value")
}

/// A selectable enum variant: the dropdown shows a human-friendly [`label`]
/// while edits round-trip the literal [`value`] written to the config.
///
/// Equality is by `value` only so the `pick_list` highlights the active choice
/// regardless of how its label is formatted.
#[derive(Clone)]
struct EnumChoice {
    value: String,
    label: String,
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

/// Build a dropdown label for an enum variant. Numeric "mode" literals (and the
/// empty/unset literal) are cryptic on their own, so their meaning is shown
/// inline (e.g. `1  ·  Follow`); self-describing textual variants (e.g.
/// `dwindle`) are shown as-is, with the full description in the helper line.
fn enum_choice_label(name: &str, description: Option<&str>) -> String {
    let shown = if name.is_empty() { "(unset)" } else { name };
    match description {
        Some(desc) if name.is_empty() || name.parse::<f64>().is_ok() => {
            format!("{shown}  ·  {}", desc_headline(desc))
        }
        _ => shown.to_string(),
    }
}

/// The leading clause of a variant description, for compact dropdown entries.
fn desc_headline(description: &str) -> &str {
    description
        .split([':', '.', '('])
        .next()
        .unwrap_or(description)
        .trim()
}

fn enum_editor(opt: &OptionSpec, loaded: &Loaded) -> Element<'static, Message> {
    let path = opt.path.clone();
    let variants = opt.enum_variants().unwrap_or_default();

    // The literal currently in effect (an explicit selection, or the default).
    // A value Hyprland accepts but we don't enumerate (e.g. a plugin-provided
    // layout) arrives as a `String`; keep it visible and selectable.
    let current = match loaded.value_for(opt) {
        Value::Enum(name) => name,
        Value::String(s) => s,
        other => value_to_conf(&other),
    };

    let mut choices: Vec<EnumChoice> = variants
        .iter()
        .map(|v| EnumChoice {
            value: v.name.clone(),
            label: enum_choice_label(&v.name, v.description.as_deref()),
        })
        .collect();
    if !choices.iter().any(|c| c.value == current) {
        let label = if current.is_empty() {
            "(unset)".to_string()
        } else {
            current.clone()
        };
        choices.insert(
            0,
            EnumChoice {
                value: current.clone(),
                label,
            },
        );
    }

    let selected = choices.iter().find(|c| c.value == current).cloned();
    let selected_desc = variants
        .iter()
        .find(|v| v.name == current)
        .and_then(|v| v.description.clone());

    let picker = pick_list(choices, selected, move |c: EnumChoice| {
        Message::Edit(EditAction::SetEnum(path.clone(), c.value))
    })
    .padding([6, 10])
    .text_size(14)
    .width(Length::Fixed(300.0));

    let mut col = column![picker].spacing(6);
    if let Some(desc) = selected_desc {
        col = col.push(text(desc).size(11).style(muted));
    }
    col.into()
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

    let top = row![
        swatch_button(color, path.clone()),
        text_field(
            &hex_draft,
            &path,
            Slot::Hex,
            hex_err,
            Length::Fixed(190.0),
            "rgba(rrggbbaa)"
        ),
        pick_button(path.clone()),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let sliders = column![
        channel_slider(&path, ColorChannel::R, "R", color.r),
        channel_slider(&path, ColorChannel::G, "G", color.g),
        channel_slider(&path, ColorChannel::B, "B", color.b),
        channel_slider(&path, ColorChannel::A, "A", color.a),
    ]
    .spacing(4);

    column![top, sliders].spacing(10).into()
}

fn channel_slider(
    path: &str,
    channel: ColorChannel,
    label: &'static str,
    value: u8,
) -> Element<'static, Message> {
    let p = path.to_string();
    row![
        text(label).size(12).style(muted).width(Length::Fixed(14.0)),
        slider(0.0..=255.0, value as f64, move |v| {
            Message::Edit(EditAction::SetColorChannel(
                p.clone(),
                channel,
                v.round() as u8,
            ))
        })
        .step(1.0)
        .width(Length::Fill),
        text(value.to_string()).size(12).width(Length::Fixed(32.0)),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

fn gradient_editor(opt: &OptionSpec, loaded: &Loaded) -> Element<'static, Message> {
    let path = opt.path.clone();
    let gradient = match loaded.value_for(opt) {
        Value::Gradient(g) => g,
        _ => return text("(invalid gradient)").size(13).style(danger).into(),
    };

    let mut rows: Vec<Element<Message>> = Vec::new();
    let count = gradient.stops.len();
    for (i, stop) in gradient.stops.iter().enumerate() {
        let draft = loaded
            .draft(&path, Slot::Stop(i))
            .map(str::to_string)
            .unwrap_or_else(|| stop.to_rgba_string());
        let err = loaded.field_error(&path, Slot::Stop(i)).is_some();
        let p = path.clone();
        let remove = if count > 1 {
            button(text("✕").size(12))
                .padding([3, 7])
                .on_press(Message::Edit(EditAction::RemoveStop(p, i)))
                .style(ghost_button)
        } else {
            button(text("✕").size(12))
                .padding([3, 7])
                .style(ghost_button)
        };
        rows.push(
            row![
                stop_swatch_button(*stop, path.clone(), i),
                text_field(&draft, &path, Slot::Stop(i), err, Length::Fill, "rgba(...)"),
                stop_pick_button(path.clone(), i),
                remove,
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into(),
        );
    }

    let add_path = path.clone();
    let add = button(text("+ add stop").size(12))
        .padding([4, 10])
        .on_press(Message::Edit(EditAction::AddStop(add_path)))
        .style(ghost_button);

    let angle_draft = loaded
        .draft(&path, Slot::Angle)
        .map(str::to_string)
        .unwrap_or_else(|| gradient.angle_deg.map(fmt_num).unwrap_or_default());
    let angle_err = loaded.field_error(&path, Slot::Angle).is_some();
    let angle_row = row![
        add,
        Space::new().width(Length::Fill),
        text("Angle°").size(12).style(muted),
        text_field(
            &angle_draft,
            &path,
            Slot::Angle,
            angle_err,
            Length::Fixed(80.0),
            "deg"
        ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    column![Column::with_children(rows).spacing(6), angle_row]
        .spacing(8)
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
        text("x").size(13).style(muted),
        text_field(&xd, &path, Slot::X, xe, Length::Fixed(110.0), ""),
        text("y").size(13).style(muted),
        text_field(&yd, &path, Slot::Y, ye, Length::Fixed(110.0), ""),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

/// A validated text input bound to a field's draft.
fn text_field(
    value: &str,
    path: &str,
    slot: Slot,
    has_error: bool,
    width: Length,
    placeholder: &str,
) -> Element<'static, Message> {
    let p = path.to_string();
    text_input(placeholder, value)
        .on_input(move |s| Message::Edit(EditAction::EditText(p.clone(), slot.clone(), s)))
        .padding([6, 8])
        .size(14)
        .width(width)
        .style(move |theme: &Theme, status| input_style(theme, status, has_error))
        .into()
}

/// A small rounded color swatch.
fn color_swatch(color: HyprColor, w: f32, h: f32) -> Element<'static, Message> {
    let fill = Color::from_rgba8(color.r, color.g, color.b, color.a as f32 / 255.0);
    container(
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
    })
    .into()
}

/// The color preview swatch, as a button that opens the visual picker.
fn swatch_button(color: HyprColor, path: String) -> Element<'static, Message> {
    button(color_swatch(color, 34.0, 24.0))
        .padding(2.0)
        .on_press(Message::OpenColorPicker(path))
        .style(ghost_button)
        .into()
}

/// The "open the visual picker" affordance next to a color field.
fn pick_button(path: String) -> Element<'static, Message> {
    button(text("🎨 pick").size(12))
        .padding([4, 10])
        .on_press(Message::OpenColorPicker(path))
        .style(ghost_button)
        .into()
}

/// A gradient stop's swatch, as a button that opens the visual picker for it.
fn stop_swatch_button(color: HyprColor, path: String, index: usize) -> Element<'static, Message> {
    button(color_swatch(color, 30.0, 22.0))
        .padding(2.0)
        .on_press(Message::OpenStopColorPicker(path, index))
        .style(ghost_button)
        .into()
}

/// The "open the visual picker" icon for a gradient stop.
fn stop_pick_button(path: String, index: usize) -> Element<'static, Message> {
    button(text("🎨").size(12))
        .padding([4, 8])
        .on_press(Message::OpenStopColorPicker(path, index))
        .style(ghost_button)
        .into()
}

// ---------------------------------------------------------------------------
// color picker popup
// ---------------------------------------------------------------------------

/// The floating color-picker panel: a saturation/value square, a hue strip, a
/// live preview, and synced HEX + R/G/B/A inputs. Every control edits the model
/// in real time (and live-applies to Hyprland when enabled).
fn color_picker_panel(app: &App, draft: &color_picker::ColorDraft) -> Element<'static, Message> {
    let target = draft.target.clone();
    let path = target.path().to_string();
    let hex_slot = match &target {
        color_picker::ColorTarget::Option(_) => Slot::Hex,
        color_picker::ColorTarget::Stop { index, .. } => Slot::Stop(*index),
    };
    let loaded = app.load.loaded();

    let color = loaded
        .and_then(|l| l.config.get(&path))
        .and_then(|v| match &target {
            color_picker::ColorTarget::Option(_) => value_to_color(v),
            color_picker::ColorTarget::Stop { index, .. } => value_stop(v, *index),
        })
        .unwrap_or_else(|| {
            HyprColor::from_hsv(
                f64::from(draft.hue),
                f64::from(draft.sat),
                f64::from(draft.val),
                255,
            )
        });

    let hex_draft = loaded
        .and_then(|l| l.draft(&path, hex_slot.clone()))
        .map(str::to_string)
        .unwrap_or_else(|| color.to_rgba_string());
    let hex_err = loaded
        .map(|l| l.field_error(&path, hex_slot.clone()).is_some())
        .unwrap_or(false);

    let area = canvas(color_picker::SvSquare {
        hue: draft.hue,
        sat: draft.sat,
        val: draft.val,
    })
    .width(Length::Fixed(232.0))
    .height(Length::Fixed(180.0));

    let hue = canvas(color_picker::HueStrip { hue: draft.hue })
        .width(Length::Fixed(24.0))
        .height(Length::Fixed(180.0));

    let fill = Color::from_rgba8(color.r, color.g, color.b, f32::from(color.a) / 255.0);
    let preview = container(Space::new().width(Length::Fill).height(Length::Fixed(40.0))).style(
        move |theme: &Theme| container::Style {
            background: Some(fill.into()),
            border: Border {
                color: theme.extended_palette().background.strong.color,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        },
    );

    let controls = column![
        preview,
        text_field(
            &hex_draft,
            &path,
            hex_slot,
            hex_err,
            Length::Fill,
            "rgba(rrggbbaa)"
        ),
        popup_channel_slider(&target, color, ColorChannel::R, "R", color.r),
        popup_channel_slider(&target, color, ColorChannel::G, "G", color.g),
        popup_channel_slider(&target, color, ColorChannel::B, "B", color.b),
        popup_channel_slider(&target, color, ColorChannel::A, "A", color.a),
    ]
    .spacing(8)
    .width(Length::Fixed(230.0));

    let header = row![
        text("Color picker").size(16).font(BOLD),
        Space::new().width(Length::Fill),
        button(text("✕").size(14))
            .padding([2, 8])
            .on_press(Message::CloseColorPicker)
            .style(ghost_button),
    ]
    .align_y(Alignment::Center);

    let footer = row![
        text(path).size(11).font(MONO).style(muted),
        Space::new().width(Length::Fill),
        button(text("Done").size(13))
            .padding([6, 16])
            .on_press(Message::CloseColorPicker)
            .style(changes_button_style),
    ]
    .align_y(Alignment::Center);

    container(column![header, row![area, hue, controls].spacing(14), footer,].spacing(14))
        .padding(18)
        .max_width(540.0)
        .style(card_style)
        .into()
}

/// A color channel slider inside the popup. Unlike the inline `channel_slider`
/// (which targets a scalar color option), this builds a full-color edit so it
/// works for both scalar options and gradient stops.
fn popup_channel_slider(
    target: &color_picker::ColorTarget,
    color: HyprColor,
    channel: ColorChannel,
    label: &'static str,
    value: u8,
) -> Element<'static, Message> {
    let path = target.path().to_string();
    let index = match target {
        color_picker::ColorTarget::Stop { index, .. } => Some(*index),
        color_picker::ColorTarget::Option(_) => None,
    };
    row![
        text(label).size(12).style(muted).width(Length::Fixed(14.0)),
        slider(0.0..=255.0, f64::from(value), move |v| {
            let mut c = color;
            let v = v.round() as u8;
            match channel {
                ColorChannel::R => c.r = v,
                ColorChannel::G => c.g = v,
                ColorChannel::B => c.b = v,
                ColorChannel::A => c.a = v,
            }
            let action = match index {
                Some(i) => EditAction::SetStopColor(path.clone(), i, c),
                None => EditAction::SetColor(path.clone(), c),
            };
            Message::Edit(action)
        })
        .step(1.0)
        .width(Length::Fill),
        text(value.to_string()).size(12).width(Length::Fixed(32.0)),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

/// Extract a [`HyprColor`] from a [`Value::Color`].
fn value_to_color(value: &Value) -> Option<HyprColor> {
    match value {
        Value::Color(c) => Some(*c),
        _ => None,
    }
}

/// Extract one stop from a [`Value::Gradient`].
fn value_stop(value: &Value, index: usize) -> Option<HyprColor> {
    match value {
        Value::Gradient(g) => g.stops.get(index).copied(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// pending changes view
// ---------------------------------------------------------------------------

fn changes_view(app: &App, loaded: &Loaded) -> Element<'static, Message> {
    let diff = loaded.pending_diff();
    let touched = loaded.touched_collections();
    let total = diff.len() + touched.len();
    let trailing = format!("{total} change{}", if total == 1 { "" } else { "s" });

    let mut items: Vec<Element<Message>> = vec![
        pane_header(
            "✎",
            "Pending changes".to_string(),
            "Unsaved edits relative to the loaded file.".to_string(),
            trailing,
        ),
        Space::new().height(Length::Fixed(4.0)).into(),
    ];

    if total == 0 {
        items.push(
            container(text("No unsaved changes.").size(14).style(muted))
                .padding([10, 14])
                .into(),
        );
    }

    for id in touched {
        let label = app
            .schema
            .collection(id)
            .map(|c| c.label.clone())
            .unwrap_or_default();
        let count = collection_count(app, id);
        items.push(
            container(
                row![
                    text(format!("{} {}", collection_icon(id), label)).size(14),
                    Space::new().width(Length::Fill),
                    text(format!("edited · {count} entries"))
                        .size(12)
                        .style(accent),
                ]
                .align_y(Alignment::Center),
            )
            .padding([10, 14])
            .width(Length::Fill)
            .style(card_style)
            .into(),
        );
    }

    for (path, old, new) in diff {
        let reset_path = path.clone();
        let row = row![
            column![
                text(path).size(14),
                row![
                    text(old).size(12).style(muted),
                    text("→").size(12).style(muted),
                    text(new).size(12).style(accent),
                ]
                .spacing(8),
            ]
            .spacing(3)
            .width(Length::Fill),
            button(text("↺ reset").size(12))
                .padding([3, 8])
                .on_press(Message::Edit(EditAction::Reset(reset_path)))
                .style(ghost_button),
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        items.push(
            container(row)
                .padding([10, 14])
                .width(Length::Fill)
                .style(card_style)
                .into(),
        );
    }

    scroll(items)
}

// ---------------------------------------------------------------------------
// profiles & recent files
// ---------------------------------------------------------------------------

fn profiles_view(app: &App) -> Element<'static, Message> {
    let mut items: Vec<Element<Message>> = vec![pane_header(
        "🗂",
        "Profiles & recents".to_string(),
        "Save the current config as a named profile, reopen recents, or import any file."
            .to_string(),
        String::new(),
    )];

    // Save-as card (only meaningful with a loaded config).
    if app.load.loaded().is_some() {
        let mut save_btn = button(text("save profile").size(13))
            .padding([6, 12])
            .style(changes_button_style);
        if !app.profile_name.trim().is_empty() {
            save_btn = save_btn.on_press(Message::SaveProfile);
        }
        let mut card = column![
            text("Save current as profile").size(14).font(BOLD),
            row![
                text_input("profile name", &app.profile_name)
                    .on_input(Message::ProfileNameChanged)
                    .padding([6, 8])
                    .size(14)
                    .width(Length::Fill),
                save_btn,
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(8);
        if let Some(status) = &app.save_status {
            match status {
                Ok(msg) => card = card.push(text(format!("✓ {msg}")).size(12).style(success)),
                Err(msg) => card = card.push(text(format!("✕ {msg}")).size(12).style(danger)),
            }
        }
        items.push(
            container(card)
                .padding([12, 16])
                .width(Length::Fill)
                .style(card_style)
                .into(),
        );
    }

    // Saved profiles.
    let mut saved = column![text("Saved profiles").size(14).font(BOLD)].spacing(8);
    let profiles = crate::profiles::list();
    if profiles.is_empty() {
        saved = saved.push(text("No saved profiles yet.").size(12).style(muted));
    }
    for profile in profiles {
        saved = saved.push(
            row![
                text(profile.name).size(14).width(Length::Fill),
                button(text("open").size(12))
                    .padding([3, 10])
                    .on_press(Message::OpenPath(profile.path))
                    .style(ghost_button),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    items.push(
        container(saved)
            .padding([12, 16])
            .width(Length::Fill)
            .style(card_style)
            .into(),
    );

    // Recent files.
    let mut recents = column![text("Recent files").size(14).font(BOLD)].spacing(6);
    if app.settings.recent_files.is_empty() {
        recents = recents.push(text("No recent files.").size(12).style(muted));
    }
    for recent in &app.settings.recent_files {
        recents = recents.push(
            button(text(recent.clone()).size(13).font(MONO))
                .width(Length::Fill)
                .padding([6, 10])
                .on_press(Message::OpenPath(std::path::PathBuf::from(recent)))
                .style(result_style),
        );
    }
    items.push(
        container(recents)
            .padding([12, 16])
            .width(Length::Fill)
            .style(card_style)
            .into(),
    );

    // Import from an arbitrary path.
    let trimmed = app.import_path.trim().to_string();
    let mut import_btn = button(text("load").size(13))
        .padding([6, 12])
        .style(ghost_button);
    if !trimmed.is_empty() {
        import_btn = import_btn.on_press(Message::OpenPath(std::path::PathBuf::from(trimmed)));
    }
    let import = column![
        text("Import from path").size(14).font(BOLD),
        row![
            text_input("/path/to/hyprland.conf or config.lua", &app.import_path)
                .on_input(Message::ImportPathChanged)
                .padding([6, 8])
                .size(14)
                .width(Length::Fill),
            import_btn,
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(8);
    items.push(
        container(import)
            .padding([12, 16])
            .width(Length::Fill)
            .style(card_style)
            .into(),
    );

    scroll(items)
}

// ---------------------------------------------------------------------------
// save panel
// ---------------------------------------------------------------------------

fn save_view(app: &App, loaded: &Loaded, preview: &save::SavePreview) -> Element<'static, Message> {
    let plan = &preview.plan;
    let problems = &preview.problems;
    let block_reason = save::blocked(problems, app.override_warnings);

    let mode_label = match plan.mode {
        SaveMode::Preserve => "preserve (edit in place)",
        SaveMode::Regenerate => "regenerate (fresh file)",
    };

    let subtitle = if loaded.is_multi_file() {
        format!("Mode: {mode_label}. Spans multiple files — only changed files are written.")
    } else {
        format!("Mode: {mode_label}. Review the diff, then write.")
    };
    let mut items: Vec<Element<Message>> = vec![pane_header(
        "💾",
        "Save".to_string(),
        subtitle,
        format!("{} change(s)", plan.changed_files().len()),
    )];

    // Output format selector.
    items.push(
        container(
            row![
                text("Output format").size(13).style(muted),
                format_chip(plan.format, ConfigFormat::Conf, "conf"),
                format_chip(plan.format, ConfigFormat::Lua, "Lua"),
                Space::new().width(Length::Fill),
                text(if plan.format == loaded.format {
                    String::new()
                } else {
                    format!("converting from {}", format_label(loaded.format))
                })
                .size(12)
                .style(muted),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([10, 14])
        .width(Length::Fill)
        .style(card_style)
        .into(),
    );

    // Dynamic-Lua loss warning.
    if plan.drops_dynamic > 0 {
        items.push(
            container(
                text(format!(
                    "⚠ {} dynamic Lua region(s) (loops, functions, …) cannot be represented and will be dropped.",
                    plan.drops_dynamic
                ))
                .size(13)
                .style(danger),
            )
            .padding([10, 14])
            .width(Length::Fill)
            .style(card_style)
            .into(),
        );
    }

    // Validation results.
    items.push(validation_panel(problems));

    // Override + write controls.
    let mut controls = row![].spacing(12).align_y(Alignment::Center);
    if problems.iter().any(|p| p.severity == Severity::Warning) {
        controls = controls.push(
            iced::widget::checkbox(app.override_warnings)
                .label("save anyway (ignore warnings)")
                .on_toggle(Message::ToggleOverride)
                .size(16)
                .text_size(13),
        );
    }
    controls = controls.push(Space::new().width(Length::Fill));
    if let Some(reason) = &block_reason {
        controls = controls.push(text(reason.clone()).size(12).style(danger));
    }
    let mut write = button(text("⤓ write to disk").size(14))
        .padding([8, 16])
        .style(changes_button_style);
    if block_reason.is_none() && plan.has_changes() {
        write = write.on_press(Message::PerformSave);
    }
    controls = controls.push(write);
    items.push(
        container(controls)
            .padding([6, 14])
            .width(Length::Fill)
            .into(),
    );

    if !plan.has_changes() {
        items.push(
            container(
                text("Nothing to write — the model matches what's on disk.")
                    .size(13)
                    .style(muted),
            )
            .padding([10, 14])
            .into(),
        );
    }

    // Per-file diff/preview (precomputed in `update`, not here).
    for file_diff_data in &preview.diffs {
        items.push(file_diff(file_diff_data));
    }

    scroll(items)
}

fn format_chip(
    active: ConfigFormat,
    value: ConfigFormat,
    label: &'static str,
) -> Element<'static, Message> {
    chip(
        label.to_string(),
        active == value,
        Message::SetOutputFormat(value),
    )
}

fn validation_panel(problems: &[save::Problem]) -> Element<'static, Message> {
    if problems.is_empty() {
        return container(text("✓ No problems found.").size(13).style(success))
            .padding([10, 14])
            .width(Length::Fill)
            .style(card_style)
            .into();
    }

    let mut rows: Vec<Element<Message>> = vec![text("Validation").size(14).font(BOLD).into()];
    for problem in problems {
        let (mark, mark_style): (&str, fn(&Theme) -> text::Style) = match problem.severity {
            Severity::Error => ("✕", danger),
            Severity::Warning => ("!", warn_style),
        };
        let mut label_btn = button(text(problem.label.clone()).size(13))
            .padding([2, 6])
            .style(ghost_button);
        if let Some(jump) = &problem.jump {
            label_btn = label_btn.on_press(Message::Selected(jump.clone()));
        }
        rows.push(
            row![
                text(mark).size(13).style(mark_style),
                label_btn,
                text(problem.message.clone()).size(12).style(muted),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into(),
        );
    }

    container(Column::with_children(rows).spacing(6))
        .padding([12, 14])
        .width(Length::Fill)
        .style(card_style)
        .into()
}

fn file_diff(file: &save::FileDiff) -> Element<'static, Message> {
    let header = row![
        text(file.path.display().to_string()).size(13).font(BOLD),
        Space::new().width(Length::Fill),
        text(format!("+{}", file.added)).size(12).style(success),
        text(format!("-{}", file.removed)).size(12).style(danger),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let mut lines: Vec<Element<Message>> = Vec::new();
    for d in &file.lines {
        let (prefix, style): (&str, fn(&Theme) -> text::Style) = match d.tag {
            Tag::Insert => ("+", success),
            Tag::Delete => ("-", danger),
            Tag::Equal => (" ", muted),
        };
        lines.push(
            text(format!("{prefix} {}", d.text))
                .size(12)
                .font(MONO)
                .style(style)
                .into(),
        );
    }

    container(
        column![
            header,
            container(Column::with_children(lines).spacing(0))
                .padding([8, 10])
                .width(Length::Fill)
                .style(code_style),
        ]
        .spacing(8),
    )
    .padding([12, 14])
    .width(Length::Fill)
    .style(card_style)
    .into()
}

/// Collections that have full row editors (others are shown read-only).
fn is_editable(id: CollectionId) -> bool {
    matches!(
        id,
        CollectionId::Keybinds
            | CollectionId::WindowRules
            | CollectionId::LayerRules
            | CollectionId::Monitors
            | CollectionId::Submaps
            | CollectionId::Env
            | CollectionId::Execs
    )
}

fn collection_view(app: &App, loaded: &Loaded, id: CollectionId) -> Element<'static, Message> {
    let (label, description) = app
        .schema
        .collection(id)
        .map(|c| (c.label.clone(), c.description.clone()))
        .unwrap_or_default();

    let count = collection_count(app, id);
    let trailing = format!("{count} entr{}", if count == 1 { "y" } else { "ies" });

    let mut items: Vec<Element<Message>> = vec![pane_header(
        collection_icon(id),
        label,
        description,
        trailing,
    )];

    if is_editable(id) {
        items.push(
            button(text(format!("+ add {}", singular(id))).size(13))
                .padding([6, 12])
                .on_press(Message::CollectionEdit(CollectionAction::Add(id)))
                .style(ghost_button)
                .into(),
        );
        items.extend(collection_rows(loaded, id, count));
        if count == 0 {
            items.push(
                container(text("No entries yet.").size(13).style(muted))
                    .padding([8, 4])
                    .into(),
            );
        }
    } else {
        // Read-only collections (workspaces / beziers / animations) for now.
        let lines = collection_lines(loaded, id);
        if lines.is_empty() {
            items.push(
                container(
                    text("No entries in this configuration.")
                        .size(13)
                        .style(muted),
                )
                .padding([10, 14])
                .into(),
            );
        }
        for line in lines {
            items.push(
                container(text(line).size(13))
                    .padding([8, 14])
                    .width(Length::Fill)
                    .style(card_style)
                    .into(),
            );
        }
    }

    scroll(items)
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
        _ => "entry",
    }
}

fn collection_rows(
    loaded: &Loaded,
    id: CollectionId,
    count: usize,
) -> Vec<Element<'static, Message>> {
    let c = &loaded.config;
    match id {
        CollectionId::Keybinds => c
            .keybinds
            .iter()
            .enumerate()
            .map(|(i, t)| keybind_row(i, &t.value, count))
            .collect(),
        CollectionId::WindowRules => c
            .window_rules
            .iter()
            .enumerate()
            .map(|(i, t)| window_rule_row(i, &t.value, count))
            .collect(),
        CollectionId::LayerRules => c
            .layer_rules
            .iter()
            .enumerate()
            .map(|(i, t)| layer_rule_row(i, &t.value, count))
            .collect(),
        CollectionId::Monitors => c
            .monitors
            .iter()
            .enumerate()
            .map(|(i, t)| monitor_row(i, &t.value, count))
            .collect(),
        CollectionId::Submaps => c
            .submaps
            .iter()
            .enumerate()
            .map(|(i, t)| submap_row(i, &t.value, count))
            .collect(),
        CollectionId::Env => c
            .env
            .iter()
            .enumerate()
            .map(|(i, t)| env_row(i, &t.value, count))
            .collect(),
        CollectionId::Execs => c
            .execs
            .iter()
            .enumerate()
            .map(|(i, t)| exec_row(i, &t.value, count))
            .collect(),
        _ => Vec::new(),
    }
}

/// The shared per-row control strip: index, reorder, duplicate, remove + issue.
fn row_controls(
    id: CollectionId,
    i: usize,
    count: usize,
    issue: Option<String>,
) -> Element<'static, Message> {
    let up = icon_button(
        "↑",
        (i > 0).then(|| Message::CollectionEdit(CollectionAction::Move(id, i, Dir::Up))),
    );
    let down = icon_button(
        "↓",
        (i + 1 < count).then(|| Message::CollectionEdit(CollectionAction::Move(id, i, Dir::Down))),
    );
    let dup = icon_button(
        "⧉",
        Some(Message::CollectionEdit(CollectionAction::Duplicate(id, i))),
    );
    let del = icon_button(
        "✕",
        Some(Message::CollectionEdit(CollectionAction::Remove(id, i))),
    );

    let mut controls = row![
        text(format!("#{}", i + 1)).size(12).style(muted),
        up,
        down,
        dup,
        Space::new().width(Length::Fill),
    ]
    .spacing(4)
    .align_y(Alignment::Center);

    if let Some(issue) = issue {
        controls = controls.push(text(format!("⚠ {issue}")).size(11).style(danger));
    }
    controls = controls.push(del);
    controls.into()
}

fn icon_button(label: &'static str, message: Option<Message>) -> Element<'static, Message> {
    let mut b = button(text(label).size(13))
        .padding([2, 7])
        .style(ghost_button);
    if let Some(m) = message {
        b = b.on_press(m);
    }
    b.into()
}

fn row_card(
    controls: Element<'static, Message>,
    body: Element<'static, Message>,
) -> Element<'static, Message> {
    container(column![controls, body].spacing(10))
        .padding([12, 14])
        .width(Length::Fill)
        .style(card_style)
        .into()
}

fn coll_text(
    value: &str,
    placeholder: &'static str,
    width: Length,
    make: impl Fn(String) -> Message + 'static,
) -> Element<'static, Message> {
    text_input(placeholder, value)
        .on_input(make)
        .padding([6, 8])
        .size(14)
        .width(width)
        .into()
}

fn chip(label: String, active: bool, message: Message) -> Element<'static, Message> {
    button(text(label).size(12))
        .padding([3, 9])
        .on_press(message)
        .style(move |theme: &Theme, status| chip_style(theme, status, active))
        .into()
}

fn keybind_row(i: usize, kb: &Keybind, count: usize) -> Element<'static, Message> {
    let mods = row(MODS.iter().map(|&name| {
        let active = has_mod(&kb.mods, name);
        chip(
            name.to_string(),
            active,
            Message::CollectionEdit(CollectionAction::Keybind(
                i,
                KeybindEdit::ToggleMod(name.to_string(), !active),
            )),
        )
    }))
    .spacing(6);

    let key = coll_text(&kb.key, "key", Length::Fixed(110.0), move |s| {
        Message::CollectionEdit(CollectionAction::Keybind(i, KeybindEdit::Key(s)))
    });

    let mut options: Vec<String> = DISPATCHERS.iter().map(|s| s.to_string()).collect();
    if !options.contains(&kb.dispatcher) && !kb.dispatcher.is_empty() {
        options.insert(0, kb.dispatcher.clone());
    }
    let dispatcher = pick_list(options, Some(kb.dispatcher.clone()), move |d| {
        Message::CollectionEdit(CollectionAction::Keybind(i, KeybindEdit::Dispatcher(d)))
    })
    .text_size(14)
    .padding([6, 10])
    .width(Length::Fixed(200.0));

    let args = coll_text(&kb.args, "arguments", Length::Fill, move |s| {
        Message::CollectionEdit(CollectionAction::Keybind(i, KeybindEdit::Args(s)))
    });

    let flags = row![
        flag_chip(i, "m", BindFlag::Mouse, kb.flags.mouse),
        flag_chip(i, "e", BindFlag::Repeat, kb.flags.repeat),
        flag_chip(i, "r", BindFlag::Release, kb.flags.release),
        flag_chip(i, "l", BindFlag::Locked, kb.flags.locked),
        flag_chip(i, "n", BindFlag::NonConsuming, kb.flags.non_consuming),
        flag_chip(i, "t", BindFlag::Transparent, kb.flags.transparent),
        flag_chip(i, "i", BindFlag::IgnoreMods, kb.flags.ignore_mods),
        Space::new().width(Length::Fill),
        text("submap").size(12).style(muted),
        coll_text(
            kb.submap.as_deref().unwrap_or(""),
            "global",
            Length::Fixed(130.0),
            move |s| Message::CollectionEdit(CollectionAction::Keybind(i, KeybindEdit::Submap(s))),
        ),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    let body = column![
        mods,
        row![key, dispatcher, args]
            .spacing(8)
            .align_y(Alignment::Center),
        flags,
    ]
    .spacing(8);

    row_card(
        row_controls(CollectionId::Keybinds, i, count, keybind_issue(kb)),
        body.into(),
    )
}

fn flag_chip(
    i: usize,
    label: &'static str,
    flag: BindFlag,
    active: bool,
) -> Element<'static, Message> {
    chip(
        label.to_string(),
        active,
        Message::CollectionEdit(CollectionAction::Keybind(
            i,
            KeybindEdit::Flag(flag, !active),
        )),
    )
}

fn window_rule_row(i: usize, wr: &WindowRule, count: usize) -> Element<'static, Message> {
    let v2 = chip(
        "v2".to_string(),
        wr.v2,
        Message::CollectionEdit(CollectionAction::WindowRule(i, WindowRuleEdit::V2(!wr.v2))),
    );
    let rule = coll_text(
        &wr.rule,
        "rule (e.g. float, opacity 0.9)",
        Length::Fill,
        move |s| Message::CollectionEdit(CollectionAction::WindowRule(i, WindowRuleEdit::Rule(s))),
    );

    let mut match_rows: Vec<Element<Message>> = Vec::new();
    for (mi, (key, value)) in parse_matchers(&wr.matchers).into_iter().enumerate() {
        let k = coll_text(
            &key,
            "class / title / …",
            Length::Fixed(150.0),
            move |s| {
                Message::CollectionEdit(CollectionAction::WindowRule(
                    i,
                    WindowRuleEdit::MatchKey(mi, s),
                ))
            },
        );
        let v = coll_text(&value, "match value", Length::Fill, move |s| {
            Message::CollectionEdit(CollectionAction::WindowRule(
                i,
                WindowRuleEdit::MatchValue(mi, s),
            ))
        });
        let del = icon_button(
            "✕",
            Some(Message::CollectionEdit(CollectionAction::WindowRule(
                i,
                WindowRuleEdit::RemoveMatch(mi),
            ))),
        );
        match_rows.push(
            row![k, text(":").size(13).style(muted), v, del]
                .spacing(6)
                .align_y(Alignment::Center)
                .into(),
        );
    }
    let add_match = button(text("+ match criterion").size(12))
        .padding([4, 10])
        .on_press(Message::CollectionEdit(CollectionAction::WindowRule(
            i,
            WindowRuleEdit::AddMatch,
        )))
        .style(ghost_button);

    // Raw escape hatch for v1 regexes / matchers with commas the builder can't model.
    let raw = coll_text(&wr.matchers, "raw matchers", Length::Fill, move |s| {
        Message::CollectionEdit(CollectionAction::WindowRule(i, WindowRuleEdit::Matchers(s)))
    });

    let body = column![
        row![text("type").size(12).style(muted), v2, rule]
            .spacing(8)
            .align_y(Alignment::Center),
        text("match criteria").size(12).style(muted),
        Column::with_children(match_rows).spacing(6),
        add_match,
        row![text("raw").size(12).style(muted), raw]
            .spacing(8)
            .align_y(Alignment::Center),
    ]
    .spacing(8);

    row_card(
        row_controls(CollectionId::WindowRules, i, count, window_rule_issue(wr)),
        body.into(),
    )
}

fn layer_rule_row(i: usize, lr: &LayerRule, count: usize) -> Element<'static, Message> {
    let rule = coll_text(
        &lr.rule,
        "rule (e.g. blur)",
        Length::Fixed(220.0),
        move |s| Message::CollectionEdit(CollectionAction::LayerRule(i, LayerRuleEdit::Rule(s))),
    );
    let ns = coll_text(
        &lr.namespace,
        "namespace (e.g. waybar)",
        Length::Fill,
        move |s| {
            Message::CollectionEdit(CollectionAction::LayerRule(i, LayerRuleEdit::Namespace(s)))
        },
    );
    let body = row![rule, text("⟵").size(13).style(muted), ns]
        .spacing(8)
        .align_y(Alignment::Center);
    row_card(
        row_controls(CollectionId::LayerRules, i, count, layer_rule_issue(lr)),
        body.into(),
    )
}

/// A labelled dropdown for a fixed-choice collection field (e.g. a monitor
/// transform). Always offers an explicit "(unset)" entry and keeps any unknown
/// current value selectable, mirroring [`enum_editor`].
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
            .text_size(14)
            .width(Length::Fixed(width)),
    ]
    .spacing(2)
    .width(Length::Fixed(width))
    .into()
}

fn monitor_row(i: usize, m: &MonitorRule, count: usize) -> Element<'static, Message> {
    let field = |label: &'static str,
                 value: String,
                 placeholder: &'static str,
                 make: fn(String) -> MonitorEdit| {
        column![
            text(label).size(11).style(muted),
            text_input(placeholder, &value)
                .on_input(move |s| Message::CollectionEdit(CollectionAction::Monitor(i, make(s))))
                .padding([6, 8])
                .size(14),
        ]
        .spacing(2)
    };

    let body = column![
        row![
            field(
                "connector",
                m.name.clone(),
                "DP-1 / desc:…",
                MonitorEdit::Name
            )
            .width(Length::FillPortion(2)),
            field("mode", m.mode.clone(), "1920x1080@144", MonitorEdit::Mode)
                .width(Length::FillPortion(2)),
        ]
        .spacing(10),
        row![
            field(
                "position",
                m.position.clone(),
                "0x0 / auto",
                MonitorEdit::Position
            )
            .width(Length::FillPortion(2)),
            field("scale", m.scale.clone(), "1 / auto", MonitorEdit::Scale)
                .width(Length::Fixed(120.0)),
            coll_choice(
                "transform",
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
                move |s| Message::CollectionEdit(CollectionAction::Monitor(
                    i,
                    MonitorEdit::Transform(s)
                )),
            ),
            coll_choice(
                "vrr",
                extra_field(&m.extra, "vrr"),
                &[("0", "Off"), ("1", "On"), ("2", "Fullscreen only")],
                160.0,
                move |s| Message::CollectionEdit(CollectionAction::Monitor(i, MonitorEdit::Vrr(s))),
            ),
            field(
                "mirror",
                extra_field(&m.extra, "mirror"),
                "DP-2",
                MonitorEdit::Mirror
            )
            .width(Length::Fixed(110.0)),
        ]
        .spacing(10),
    ]
    .spacing(8);

    row_card(
        row_controls(CollectionId::Monitors, i, count, monitor_issue(m)),
        body.into(),
    )
}

fn submap_row(i: usize, s: &Submap, count: usize) -> Element<'static, Message> {
    let name = coll_text(&s.name, "submap name", Length::Fixed(240.0), move |v| {
        Message::CollectionEdit(CollectionAction::Submap(i, v))
    });
    let body = row![text("name").size(12).style(muted), name]
        .spacing(8)
        .align_y(Alignment::Center);
    row_card(
        row_controls(CollectionId::Submaps, i, count, None),
        body.into(),
    )
}

fn env_row(i: usize, e: &EnvVar, count: usize) -> Element<'static, Message> {
    let name = coll_text(&e.name, "NAME", Length::Fixed(220.0), move |s| {
        Message::CollectionEdit(CollectionAction::Env(i, EnvEdit::Name(s)))
    });
    let value = coll_text(&e.value, "value", Length::Fill, move |s| {
        Message::CollectionEdit(CollectionAction::Env(i, EnvEdit::Value(s)))
    });
    let body = row![name, text("=").size(13).style(muted), value]
        .spacing(8)
        .align_y(Alignment::Center);
    row_card(
        row_controls(CollectionId::Env, i, count, env_issue(e)),
        body.into(),
    )
}

fn exec_row(i: usize, e: &Exec, count: usize) -> Element<'static, Message> {
    let kinds = vec![
        "exec-once".to_string(),
        "exec".to_string(),
        "exec-shutdown".to_string(),
    ];
    let current = match e.kind {
        ExecKind::Exec => "exec",
        ExecKind::ExecOnce => "exec-once",
        ExecKind::ExecShutdown => "exec-shutdown",
    }
    .to_string();
    let kind = pick_list(kinds, Some(current), move |label| {
        let kind = match label.as_str() {
            "exec" => ExecKind::Exec,
            "exec-shutdown" => ExecKind::ExecShutdown,
            _ => ExecKind::ExecOnce,
        };
        Message::CollectionEdit(CollectionAction::Exec(i, ExecEdit::Kind(kind)))
    })
    .text_size(14)
    .padding([6, 10])
    .width(Length::Fixed(150.0));

    let command = coll_text(&e.command, "command", Length::Fill, move |s| {
        Message::CollectionEdit(CollectionAction::Exec(i, ExecEdit::Command(s)))
    });

    let body = row![kind, command].spacing(8).align_y(Alignment::Center);
    row_card(
        row_controls(CollectionId::Execs, i, count, exec_issue(e)),
        body.into(),
    )
}

fn search_results(app: &App, loaded: &Loaded) -> Element<'static, Message> {
    let query = app.search.trim();

    let mut scored: Vec<(i32, &str, &OptionSpec)> = Vec::new();
    for section in app.schema.sections() {
        for opt in &section.options {
            if let Some(score) = fuzzy::option_score(query, &opt.label, &opt.path, &opt.description)
            {
                scored.push((score, section.id.as_str(), opt));
            }
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.path.cmp(&b.2.path)));

    let mut items: Vec<Element<Message>> = vec![text(format!(
        "{} option{} matching “{query}”",
        scored.len(),
        if scored.len() == 1 { "" } else { "s" }
    ))
    .size(13)
    .style(muted)
    .into()];

    if scored.is_empty() {
        items.push(text("No matches.").size(15).into());
    }

    for (_score, section_id, opt) in scored.into_iter().take(300) {
        let value = match loaded.config.get(&opt.path) {
            Some(v) => render_value(v),
            None => render_value(&opt.default),
        };
        let inner = row![
            row![
                text(section_icon(section_id)).size(13),
                text(opt.label.clone()).size(14)
            ]
            .spacing(8)
            .width(Length::FillPortion(3)),
            container(text(value).size(13).style(accent))
                .width(Length::FillPortion(2))
                .align_x(Alignment::End),
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        items.push(
            button(inner)
                .width(Length::Fill)
                .padding([8, 14])
                .on_press(Message::Selected(Selection::Section(
                    section_id.to_string(),
                )))
                .style(result_style)
                .into(),
        );
    }

    scroll(items)
}

/// A vertical, scrollable, fill-width column.
fn scroll(items: Vec<Element<'static, Message>>) -> Element<'static, Message> {
    scrollable(
        Column::with_children(items)
            .spacing(8)
            .width(Length::Fill)
            .padding([0, 8]),
    )
    .height(Length::Fill)
    .into()
}

// ---------------------------------------------------------------------------
// status bar
// ---------------------------------------------------------------------------

fn status_bar(app: &App) -> Element<'_, Message> {
    let content: Element<Message> = match &app.load {
        LoadState::Loading => text("Loading…").size(12).style(muted).into(),
        LoadState::NotFound { .. } => text("No configuration loaded").size(12).style(muted).into(),
        LoadState::Error { .. } => text("Error loading configuration")
            .size(12)
            .style(danger)
            .into(),
        LoadState::Loaded(loaded) => {
            let mut segs = row![
                container(text(format_label(loaded.format)).size(11).font(BOLD))
                    .padding([1, 8])
                    .style(format_badge_style),
                text(loaded.source.display().to_string())
                    .size(12)
                    .style(muted),
            ]
            .spacing(10)
            .align_y(Alignment::Center);

            if loaded.included_files > 0 {
                segs = segs.push(
                    text(format!("+{} included", loaded.included_files))
                        .size(12)
                        .style(muted),
                );
            }
            segs = segs.push(Space::new().width(Length::Fill));
            segs = segs.push(hyprland_status(app));
            if let Some(info) = &app.hyprland {
                let stale =
                    hyprconf_core::unsupported_options(app.schema, &loaded.config, &info.version);
                if !stale.is_empty() {
                    segs = segs.push(
                        text(format!("⚠ {} need newer Hyprland", stale.len()))
                            .size(12)
                            .style(warn_style),
                    );
                }
            }
            if let Some(status) = &app.save_status {
                match status {
                    Ok(msg) => segs = segs.push(text(format!("✓ {msg}")).size(12).style(success)),
                    Err(msg) => segs = segs.push(text(format!("✕ {msg}")).size(12).style(danger)),
                }
            }
            segs = segs.push(
                text(format!("{} options set", loaded.config.option_count()))
                    .size(12)
                    .style(muted),
            );
            if loaded.warnings > 0 {
                segs = segs.push(
                    text(format!("⚠ {} warnings", loaded.warnings))
                        .size(12)
                        .style(danger),
                );
            }
            segs.into()
        }
    };

    container(content)
        .width(Length::Fill)
        .padding([6, 16])
        .style(bar_style)
        .into()
}

/// The Hyprland indicator: version badge + live-apply toggle + reload, or a
/// muted "not detected" note. Degrades gracefully when `hyprctl` is absent.
fn hyprland_status(app: &App) -> Element<'_, Message> {
    let Some(info) = &app.hyprland else {
        return text("Hyprland: not detected").size(12).style(muted).into();
    };

    let reload = button(text("⟳ reload").size(12))
        .padding([2, 8])
        .on_press(Message::Reload)
        .style(ghost_button);

    let mut strip = row![
        container(
            text(format!("Hyprland {}", info.version))
                .size(11)
                .font(BOLD)
        )
        .padding([1, 8])
        .style(format_badge_style),
        text("live").size(12).style(muted),
        toggler(app.live_apply)
            .on_toggle(Message::ToggleLiveApply)
            .size(16),
        reload,
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    if let Some(result) = &app.hypr_status {
        match result {
            Ok(_) => strip = strip.push(text("✓").size(12).style(success)),
            Err(_) => strip = strip.push(text("✕").size(12).style(danger)),
        }
    }
    strip.into()
}

// ---------------------------------------------------------------------------
// value & collection rendering
// ---------------------------------------------------------------------------

fn render_value(value: &Value) -> String {
    value_to_conf(value)
}

fn collection_lines(loaded: &Loaded, id: CollectionId) -> Vec<String> {
    let c = &loaded.config;
    match id {
        CollectionId::Monitors => c.monitors.iter().map(|t| monitor_line(&t.value)).collect(),
        CollectionId::Workspaces => c
            .workspaces
            .iter()
            .map(|t| workspace_line(&t.value))
            .collect(),
        CollectionId::WindowRules => c
            .window_rules
            .iter()
            .map(|t| window_rule_line(&t.value))
            .collect(),
        CollectionId::LayerRules => c
            .layer_rules
            .iter()
            .map(|t| layer_rule_line(&t.value))
            .collect(),
        CollectionId::Keybinds => c.keybinds.iter().map(|t| keybind_line(&t.value)).collect(),
        CollectionId::Submaps => c.submaps.iter().map(|t| submap_line(&t.value)).collect(),
        CollectionId::Env => c.env.iter().map(|t| env_line(&t.value)).collect(),
        CollectionId::Execs => c.execs.iter().map(|t| exec_line(&t.value)).collect(),
        CollectionId::Variables => c
            .variables
            .iter()
            .map(|t| variable_line(&t.value))
            .collect(),
        CollectionId::Beziers => c.beziers.iter().map(|t| bezier_line(&t.value)).collect(),
        CollectionId::Animations => c
            .animations
            .iter()
            .map(|t| animation_line(&t.value))
            .collect(),
    }
}

fn keybind_line(k: &Keybind) -> String {
    let mods = if k.mods.is_empty() {
        String::new()
    } else {
        format!("{} ", k.mods)
    };
    let args = if k.args.is_empty() {
        String::new()
    } else {
        format!(" {}", k.args)
    };
    let submap = k
        .submap
        .as_ref()
        .map(|s| format!("   [submap: {s}]"))
        .unwrap_or_default();
    format!(
        "{} · {mods}{} → {}{args}{submap}",
        k.flags.keyword(),
        k.key,
        k.dispatcher
    )
}

fn window_rule_line(r: &WindowRule) -> String {
    let kw = if r.v2 { "windowrulev2" } else { "windowrule" };
    format!("{kw} · {}  ⟵  {}", r.rule, r.matchers)
}

fn layer_rule_line(r: &LayerRule) -> String {
    format!("{}  ⟵  {}", r.rule, r.namespace)
}

fn monitor_line(m: &MonitorRule) -> String {
    let name = if m.name.is_empty() { "(all)" } else { &m.name };
    let extra = if m.extra.is_empty() {
        String::new()
    } else {
        format!("  {}", m.extra.join(", "))
    };
    format!("{name}: {} @ {} ×{}{extra}", m.mode, m.position, m.scale)
}

fn workspace_line(w: &WorkspaceRule) -> String {
    format!("{}: {}", w.selector, w.rules)
}

fn env_line(e: &EnvVar) -> String {
    format!("{} = {}", e.name, e.value)
}

fn exec_line(e: &Exec) -> String {
    let kind = match e.kind {
        ExecKind::Exec => "exec",
        ExecKind::ExecOnce => "exec-once",
        ExecKind::ExecShutdown => "exec-shutdown",
    };
    format!("{kind} · {}", e.command)
}

fn submap_line(s: &Submap) -> String {
    s.name.clone()
}

fn variable_line(v: &Variable) -> String {
    format!("${} = {}", v.name, v.value)
}

fn bezier_line(b: &Bezier) -> String {
    format!(
        "{}: ({}, {}) ({}, {})",
        b.name, b.p0.x, b.p0.y, b.p1.x, b.p1.y
    )
}

fn animation_line(a: &Animation) -> String {
    let onoff = if a.enabled { "on" } else { "off" };
    let style = a
        .style
        .as_deref()
        .map(|s| format!(", {s}"))
        .unwrap_or_default();
    format!("{}: {onoff}, speed {}, {}{style}", a.name, a.speed, a.curve)
}

// ---------------------------------------------------------------------------
// icons
// ---------------------------------------------------------------------------

fn section_icon(id: &str) -> &'static str {
    match id {
        "general" => "🪟",
        "decoration" => "🎨",
        "animations" => "✨",
        "input" => "⌨",
        "gestures" => "✋",
        "group" => "🗂",
        "misc" => "🧩",
        "binds" => "🎹",
        "dwindle" => "🌿",
        "master" => "📐",
        "xwayland" => "🩹",
        "cursor" => "🖱",
        "render" => "🖼",
        "debug" => "🐞",
        "layout" => "🧱",
        "scrolling" => "📜",
        "opengl" => "🧊",
        "ecosystem" => "🌱",
        "experimental" => "🧪",
        "quirks" => "🔧",
        _ => "•",
    }
}

fn collection_icon(id: CollectionId) -> &'static str {
    match id {
        CollectionId::Monitors => "🖥",
        CollectionId::Workspaces => "🔳",
        CollectionId::WindowRules => "📏",
        CollectionId::LayerRules => "🧅",
        CollectionId::Keybinds => "⌨",
        CollectionId::Submaps => "🗺",
        CollectionId::Env => "🌐",
        CollectionId::Execs => "▶",
        CollectionId::Variables => "🔣",
        CollectionId::Beziers => "〰",
        CollectionId::Animations => "🎞",
    }
}

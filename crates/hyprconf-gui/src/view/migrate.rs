// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rendering for the `.conf` -> Lua migration flow and its deprecation banner.

use iced::widget::{button, column, container, row, scrollable, text, toggler, Space};
use iced::{Alignment, Element, Length};

use hyprconf_core::lua::LuaNote;
use hyprconf_core::verify::Verdict;

use super::styles::*;
use super::{pane_header_count, BOLD, MONO};
use crate::migrate::{Migration, Step, CONF_REMOVED_IN};
use crate::{App, Message};

/// The dismissible banner shown while a `.conf` config is open.
///
/// Hyprland itself prints the deadline at startup, where it scrolls away in a
/// log nobody reads. Repeating it here — with the concrete version and a single
/// obvious next action — is the whole point.
pub fn deprecation_banner(app: &App) -> Element<'static, Message> {
    let running = app
        .hyprland
        .as_ref()
        .map_or_else(String::new, |h| format!(" You are running {}.", h.version));

    let message = column![
        row![
            text("⚠").size(15).style(warn_style),
            text(format!(
                "The .conf format will stop working in Hyprland {CONF_REMOVED_IN}"
            ))
            .size(14)
            .font(BOLD),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        text(format!(
            "Hyprland now warns that .conf support is going away.{running} \
             hyprconf can convert your config to Lua and check the result with \
             Hyprland before writing anything."
        ))
        .size(12)
        .style(muted),
    ]
    .spacing(4);

    container(
        row![
            message.width(Length::Fill),
            button(text("Convert to Lua…").size(13))
                .padding([8, 14])
                .on_press(Message::StartMigration)
                .style(primary_button),
            button(text("✕").size(13))
                .padding([8, 10])
                .on_press(Message::DismissDeprecation)
                .style(ghost_button),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding([10, 16])
    .width(Length::Fill)
    .style(banner_style)
    .into()
}

/// The full-pane migration flow.
pub fn migrate_view(app: &App, m: &Migration) -> Element<'static, Message> {
    let body: Element<Message> = match m.step {
        Step::Review => review_step(m),
        Step::Preview => preview_step(m),
        Step::Check => check_step(m),
        Step::Done => done_step(app, m),
    };

    let items = column![
        pane_header_count(
            "🚀",
            "Convert to Lua",
            format!(
                "{} → {}",
                m.source.display(),
                m.target.file_name().unwrap_or_default().to_string_lossy()
            ),
            format!("Step {} of {}", m.step.index(), Step::ALL.len()),
        ),
        stepper(m),
        body,
        footer(m),
    ]
    .spacing(12)
    .padding(18);

    scrollable(items).height(Length::Fill).into()
}

/// The step pills across the top.
fn stepper(m: &Migration) -> Element<'static, Message> {
    let current = m.step.index();
    let mut bar = row![].spacing(8).align_y(Alignment::Center);

    for (i, step) in Step::ALL.iter().enumerate() {
        let pos = i + 1;
        let done = pos < current;
        let label = if done {
            format!("✓ {}", step.title())
        } else {
            format!("{pos}. {}", step.title())
        };
        bar = bar.push(
            container(text(label).size(12))
                .padding([5, 12])
                .style(move |t| step_style(t, pos == current, done)),
        );
        if pos < Step::ALL.len() {
            bar = bar.push(text("›").size(12).style(muted));
        }
    }

    container(bar).padding([2, 4]).into()
}

// ---------------------------------------------------------------------------
// step 1: review
// ---------------------------------------------------------------------------

fn review_step(m: &Migration) -> Element<'static, Message> {
    let mut items: Vec<Element<Message>> = Vec::new();

    // What was found.
    let mut rows = column![text("What will be converted").size(14).font(BOLD)].spacing(6);
    for (label, count) in m.summary.rows() {
        rows = rows.push(
            row![
                text(label).size(13),
                Space::new().width(Length::Fill),
                text(count.to_string()).size(13).style(accent).font(BOLD),
            ]
            .align_y(Alignment::Center),
        );
    }
    items.push(
        container(rows)
            .padding([12, 16])
            .width(Length::Fill)
            .style(card_style)
            .into(),
    );

    // Anything that could not be translated exactly.
    let notes = m.blocking_notes();
    if notes.is_empty() && m.notes.is_empty() {
        items.push(
            container(
                column![
                    row![
                        text("✓").size(14).style(success),
                        text("Everything maps cleanly").size(14).font(BOLD),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                    text(
                        "Every setting, bind and rule has a direct Lua equivalent. \
                         Nothing will be approximated."
                    )
                    .size(12)
                    .style(muted),
                ]
                .spacing(4),
            )
            .padding([12, 16])
            .width(Length::Fill)
            .style(success_panel)
            .into(),
        );
    } else {
        items.push(
            column![
                row![
                    text("⚠").size(14).style(warn_style),
                    text(format!(
                        "{} item{} need{} a look",
                        m.notes.len(),
                        if m.notes.len() == 1 { "" } else { "s" },
                        if m.notes.len() == 1 { "s" } else { "" },
                    ))
                    .size(14)
                    .font(BOLD),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                text(
                    "These still produce a working config — they are listed so \
                     nothing changes behind your back."
                )
                .size(12)
                .style(muted),
            ]
            .spacing(4)
            .padding([4, 4])
            .into(),
        );
        for note in &m.notes {
            items.push(note_card(note));
        }
    }

    // Losing dynamic Lua only applies when going Lua -> conf, but surface it.
    if m.drops_dynamic > 0 {
        items.push(
            container(
                text(format!(
                    "⚠ {} dynamic Lua region(s) (loops, functions, …) cannot be \
                     represented in the model and would be dropped.",
                    m.drops_dynamic
                ))
                .size(12)
                .style(warn_style),
            )
            .padding([10, 14])
            .width(Length::Fill)
            .style(card_style)
            .into(),
        );
    }

    // Reassurance about what happens to the original.
    items.push(
        container(
            column![
                text("Your .conf file is not deleted").size(13).font(BOLD),
                text(format!(
                    "hyprconf writes {} and leaves {} untouched. Hyprland prefers \
                     the Lua file when both exist, so undoing the migration is just \
                     deleting the new file.",
                    m.target.file_name().unwrap_or_default().to_string_lossy(),
                    m.source.file_name().unwrap_or_default().to_string_lossy(),
                ))
                .size(12)
                .style(muted),
            ]
            .spacing(4),
        )
        .padding([12, 16])
        .width(Length::Fill)
        .style(card_style)
        .into(),
    );

    column(items).spacing(10).into()
}

fn note_card(note: &LuaNote) -> Element<'static, Message> {
    let (icon, style): (&str, fn(&iced::Theme) -> iced::widget::text::Style) = if note.lossless {
        ("⚠", warn_style)
    } else {
        ("✕", danger)
    };
    container(
        column![
            row![
                text(icon.to_string()).size(13).style(style),
                text(note.kind.to_string()).size(12).style(muted),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            text(note.subject.clone()).size(12).font(MONO),
            text(note.detail.clone()).size(12).style(muted),
        ]
        .spacing(4),
    )
    .padding([10, 14])
    .width(Length::Fill)
    .style(card_style)
    .into()
}

// ---------------------------------------------------------------------------
// step 2: preview
// ---------------------------------------------------------------------------

fn preview_step(m: &Migration) -> Element<'static, Message> {
    let lines = m.lua.lines().count();
    let mut body = column![].spacing(0);
    for (i, line) in m.lua.lines().enumerate() {
        body = body.push(
            row![
                text(format!("{:>4}", i + 1))
                    .size(11)
                    .font(MONO)
                    .style(muted)
                    .width(Length::Fixed(38.0)),
                text(line.to_string()).size(12).font(MONO),
            ]
            .spacing(8),
        );
    }

    column![
        row![
            text(format!(
                "{} — {lines} lines",
                m.target.file_name().unwrap_or_default().to_string_lossy()
            ))
            .size(13)
            .font(BOLD),
            Space::new().width(Length::Fill),
            text("nothing has been written yet").size(12).style(muted),
        ]
        .align_y(Alignment::Center),
        container(scrollable(container(body).padding(12)).height(Length::Fixed(460.0)))
            .width(Length::Fill)
            .style(code_style),
    ]
    .spacing(8)
    .into()
}

// ---------------------------------------------------------------------------
// step 3: check
// ---------------------------------------------------------------------------

fn check_step(m: &Migration) -> Element<'static, Message> {
    let explain = container(
        column![
            text("Checked by Hyprland itself").size(14).font(BOLD),
            text(
                "hyprconf runs `Hyprland --verify-config` on the generated file in \
                 a temporary directory. This is the compositor's own parser, so a \
                 pass here means the config really loads. Autostart commands are \
                 commented out for the check so nothing is launched."
            )
            .size(12)
            .style(muted),
        ]
        .spacing(4),
    )
    .padding([12, 16])
    .width(Length::Fill)
    .style(card_style);

    let verdict: Element<Message> = if m.checking {
        container(text("Checking…").size(13))
            .padding([12, 16])
            .width(Length::Fill)
            .style(card_style)
            .into()
    } else {
        match &m.verdict {
            None => container(
                row![
                    text("Not checked yet.").size(13).style(muted),
                    Space::new().width(Length::Fill),
                    button(text("Run check").size(13))
                        .padding([8, 14])
                        .on_press(Message::MigrateCheck)
                        .style(primary_button),
                ]
                .align_y(Alignment::Center),
            )
            .padding([12, 16])
            .width(Length::Fill)
            .style(card_style)
            .into(),

            Some(Verdict::Ok) => container(
                column![
                    row![
                        text("✓").size(16).style(success),
                        text("Hyprland accepted the generated config")
                            .size(14)
                            .font(BOLD),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                    text("No errors were reported. It is safe to write the file.")
                        .size(12)
                        .style(muted),
                ]
                .spacing(4),
            )
            .padding([12, 16])
            .width(Length::Fill)
            .style(success_panel)
            .into(),

            Some(Verdict::Problems(issues)) => {
                let mut list = column![row![
                    text("✕").size(16).style(danger),
                    text(format!(
                        "Hyprland reported {} problem{}",
                        issues.len(),
                        if issues.len() == 1 { "" } else { "s" }
                    ))
                    .size(14)
                    .font(BOLD),
                ]
                .spacing(8)
                .align_y(Alignment::Center),]
                .spacing(6);
                for issue in issues {
                    let where_ = issue
                        .line
                        .map_or_else(String::new, |l| format!("line {l}: "));
                    list = list.push(
                        text(format!("{where_}{}", issue.message))
                            .size(12)
                            .font(MONO),
                    );
                }
                container(list)
                    .padding([12, 16])
                    .width(Length::Fill)
                    .style(danger_panel)
                    .into()
            }

            Some(Verdict::Unavailable(reason)) => container(
                column![
                    row![
                        text("⚠").size(16).style(warn_style),
                        text("Could not machine-check the config")
                            .size(14)
                            .font(BOLD),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                    text(reason.clone()).size(12).style(muted),
                ]
                .spacing(4),
            )
            .padding([12, 16])
            .width(Length::Fill)
            .style(banner_style)
            .into(),
        }
    };

    let mut items = column![explain, verdict].spacing(10);

    // Only offer the override when the check did not pass — otherwise it is
    // noise, and an always-visible "ignore errors" toggle invites misuse.
    let needs_override = !m.checking && !matches!(m.verdict, Some(Verdict::Ok));
    if needs_override {
        items = items.push(
            container(
                column![
                    toggler(m.override_check)
                        .label("Write the file anyway")
                        .text_size(13)
                        .on_toggle(Message::MigrateOverride),
                    text(
                        "Only do this if you understand the reported problems — an \
                         invalid config can leave you without a usable desktop."
                    )
                    .size(12)
                    .style(muted),
                ]
                .spacing(4),
            )
            .padding([12, 16])
            .width(Length::Fill)
            .style(card_style),
        );
        items = items.push(
            button(text("Re-run check").size(13))
                .padding([8, 14])
                .on_press(Message::MigrateCheck)
                .style(ghost_button),
        );
    }

    items.into()
}

// ---------------------------------------------------------------------------
// step 4: done
// ---------------------------------------------------------------------------

fn done_step(app: &App, m: &Migration) -> Element<'static, Message> {
    match &m.outcome {
        None => container(
            column![
                text("Ready to write").size(14).font(BOLD),
                text(format!(
                    "hyprconf will write {} and keep your .conf as a fallback.",
                    m.target.display()
                ))
                .size(12)
                .style(muted),
            ]
            .spacing(4),
        )
        .padding([12, 16])
        .width(Length::Fill)
        .style(card_style)
        .into(),

        Some(Err(e)) => container(
            column![
                row![
                    text("✕").size(16).style(danger),
                    text("Nothing was written").size(14).font(BOLD),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                text(e.clone()).size(12),
                text("Your existing configuration is unchanged.")
                    .size(12)
                    .style(muted),
            ]
            .spacing(4),
        )
        .padding([12, 16])
        .width(Length::Fill)
        .style(danger_panel)
        .into(),

        Some(Ok(applied)) => {
            let mut items = column![
                row![
                    text("✓").size(16).style(success),
                    text("Migrated to Lua").size(16).font(BOLD),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                text(format!("Written: {}", applied.written.display())).size(12),
            ]
            .spacing(6);

            if let Some(backup) = &applied.backup {
                items = items.push(
                    text(format!("Previous file backed up: {}", backup.display()))
                        .size(12)
                        .style(muted),
                );
            }
            items = items.push(
                text(format!(
                    "Your original {} is still there. Hyprland prefers the Lua \
                     file, so delete the new file to go back.",
                    m.source.file_name().unwrap_or_default().to_string_lossy()
                ))
                .size(12)
                .style(muted),
            );

            let next = column![
                text("Next steps").size(13).font(BOLD),
                text("1. Reload Hyprland (or log out and back in) to pick up the new config.")
                    .size(12)
                    .style(muted),
                text("2. hyprconf is now editing the Lua file — further saves go there.")
                    .size(12)
                    .style(muted),
            ]
            .spacing(4);

            let mut actions = row![].spacing(8);
            if app.hyprland.is_some() {
                actions = actions.push(
                    button(text("⟳ Reload Hyprland now").size(13))
                        .padding([8, 14])
                        .on_press(Message::Reload)
                        .style(primary_button),
                );
            }
            actions = actions.push(
                button(text("Done").size(13))
                    .padding([8, 14])
                    .on_press(Message::CloseMigration)
                    .style(ghost_button),
            );

            column![
                container(items)
                    .padding([12, 16])
                    .width(Length::Fill)
                    .style(success_panel),
                container(next)
                    .padding([12, 16])
                    .width(Length::Fill)
                    .style(card_style),
                actions,
            ]
            .spacing(10)
            .into()
        }
    }
}

// ---------------------------------------------------------------------------
// footer navigation
// ---------------------------------------------------------------------------

fn footer(m: &Migration) -> Element<'static, Message> {
    // The final screen carries its own actions.
    if m.step == Step::Done && m.outcome.is_some() {
        return Space::new().height(Length::Fixed(0.0)).into();
    }

    let back = match m.step {
        Step::Review => None,
        Step::Preview => Some(Step::Review),
        Step::Check => Some(Step::Preview),
        Step::Done => Some(Step::Check),
    };

    // On the commit step, spell out what the check found right next to the
    // button that acts on it.
    let hint: Element<Message> = match (m.step, m.check_summary()) {
        (Step::Check | Step::Done, Some(summary)) => text(summary)
            .size(12)
            .style(if m.verdict.as_ref().is_some_and(Verdict::is_ok) {
                success
            } else {
                warn_style
            })
            .into(),
        _ => Space::new().into(),
    };

    let mut bar = row![
        button(text("Cancel").size(13))
            .padding([8, 14])
            .on_press(Message::CloseMigration)
            .style(ghost_button),
        Space::new().width(Length::Fill),
        hint,
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    if let Some(target) = back {
        bar = bar.push(
            button(text("← Back").size(13))
                .padding([8, 14])
                .on_press(Message::MigrateGoto(target))
                .style(ghost_button),
        );
    }

    let (label, next, enabled) = match m.step {
        Step::Review => ("Preview the Lua →", Some(Step::Preview), true),
        Step::Preview => ("Check with Hyprland →", Some(Step::Check), true),
        Step::Check => ("Write the file →", Some(Step::Done), m.can_apply()),
        Step::Done => ("Write the file", None, m.can_apply()),
    };

    let message = match (m.step, next) {
        // Advancing past the check is the commit point.
        (Step::Check, _) | (Step::Done, _) => Some(Message::MigrateApply),
        (_, Some(step)) => Some(Message::MigrateGoto(step)),
        _ => None,
    };

    let mut primary = button(text(label).size(13))
        .padding([8, 16])
        .style(primary_button);
    if enabled {
        primary = primary.on_press_maybe(message);
    }
    bar = bar.push(primary);

    container(bar).padding([4, 4]).into()
}

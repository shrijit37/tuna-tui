//! Settings overlay UI renderer.

use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{SettingControl, SettingsTab};
use crate::App;
use tuna_tui::theme::Theme;

pub(crate) fn render_settings_overlay(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let Some(state) = &app.view.settings else {
        return;
    };

    // Center an 80x24 modal box within the terminal area
    let w = (area.width.saturating_sub(4)).clamp(60, 90).min(area.width);
    let h = (area.height.saturating_sub(4))
        .clamp(18, 28)
        .min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let modal_rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    // Blank out underlying screen and paint modal panel block
    f.render_widget(Clear, modal_rect);
    f.render_widget(
        Block::default()
            .style(theme.element())
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(theme.primary.into())),
        modal_rect,
    );

    let inner = modal_rect.inner(Margin::new(2, 1));
    if inner.height < 4 || inner.width < 20 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1), // Header
        Constraint::Length(1), // Spacer
        Constraint::Min(8),    // Body (Categories on left | Options on right)
        Constraint::Length(1), // Spacer
        Constraint::Length(2), // Description / Footer
    ])
    .split(inner);

    // Header
    let header_spans = vec![
        Span::styled(
            "⚙  Settings",
            Style::default()
                .fg(theme.primary.into())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  (Tab category · ↑↓ row · ←→ change · Esc save & close)",
            theme.muted(),
        ),
    ];
    f.render_widget(Paragraph::new(Line::from(header_spans)), rows[0]);

    // Body: Split into Left Categories Sidebar and Right Options List
    let body = Layout::horizontal([
        Constraint::Length(22), // Categories
        Constraint::Min(20),    // Options
    ])
    .spacing(2)
    .split(rows[2]);

    // 1. Categories Sidebar
    let mut cat_lines = Vec::new();
    for tab in SettingsTab::ALL {
        let active = tab == state.tab;
        let style = if active {
            Style::default()
                .fg(theme.primary.into())
                .add_modifier(Modifier::BOLD)
        } else {
            theme.muted()
        };
        let prefix = if active { "▶ " } else { "  " };
        cat_lines.push(Line::from(vec![
            Span::styled(prefix, style),
            Span::styled(tab.label(), style),
        ]));
    }
    f.render_widget(
        Paragraph::new(cat_lines).block(Block::default().style(theme.element())),
        body[0],
    );

    // 2. Options List
    let current_rows = state.current_rows();
    let mut opt_lines = Vec::new();

    for (idx, row) in current_rows.iter().enumerate() {
        let is_selected = idx == state.selected;
        let row_style = if is_selected {
            Style::default()
                .fg(theme.text.into())
                .add_modifier(Modifier::BOLD)
        } else {
            theme.muted()
        };

        let pointer = if is_selected { "● " } else { "  " };
        let pointer_style = if is_selected {
            Style::default()
                .fg(theme.accent.into())
                .add_modifier(Modifier::BOLD)
        } else {
            theme.muted()
        };

        // Render control widget on right
        let control_spans = match &row.control {
            SettingControl::Toggle(val) => {
                if *val {
                    vec![
                        Span::styled(
                            "[✓] ",
                            Style::default()
                                .fg(theme.success.into())
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("Enabled", Style::default().fg(theme.text.into())),
                    ]
                } else {
                    vec![
                        Span::styled("[ ] ", theme.muted()),
                        Span::styled("Disabled", theme.muted()),
                    ]
                }
            }
            SettingControl::Choice { current, options } => {
                let opt_text = options.get(*current).map(|s| s.as_str()).unwrap_or("");
                if is_selected {
                    vec![
                        Span::styled("◀ ", Style::default().fg(theme.primary.into())),
                        Span::styled(
                            opt_text,
                            Style::default()
                                .fg(theme.text.into())
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" ▶", Style::default().fg(theme.primary.into())),
                    ]
                } else {
                    vec![
                        Span::styled("  ", theme.muted()),
                        Span::styled(opt_text, theme.muted()),
                        Span::styled("  ", theme.muted()),
                    ]
                }
            }
            SettingControl::Number { val, suffix, .. } => {
                let num_str = format!("{val}{suffix}");
                if is_selected {
                    vec![
                        Span::styled("◀ ", Style::default().fg(theme.primary.into())),
                        Span::styled(
                            num_str,
                            Style::default()
                                .fg(theme.text.into())
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" ▶", Style::default().fg(theme.primary.into())),
                    ]
                } else {
                    vec![
                        Span::styled("  ", theme.muted()),
                        Span::styled(num_str, theme.muted()),
                        Span::styled("  ", theme.muted()),
                    ]
                }
            }
            SettingControl::Action(label) => {
                if is_selected {
                    vec![Span::styled(
                        format!("[ {label} ]"),
                        Style::default()
                            .fg(theme.primary.into())
                            .add_modifier(Modifier::BOLD),
                    )]
                } else {
                    vec![Span::styled(format!("[ {label} ]"), theme.muted())]
                }
            }
            SettingControl::Separator(text) => {
                vec![Span::styled(
                    text.to_string(),
                    Style::default()
                        .fg(theme.border_active.into())
                        .add_modifier(Modifier::BOLD),
                )]
            }
        };
        let mut spans = vec![
            Span::styled(pointer, pointer_style),
            Span::styled(format!("{:<26}", row.label), row_style),
            Span::raw("  "),
        ];
        spans.extend(control_spans);
        opt_lines.push(Line::from(spans));
    }

    f.render_widget(Paragraph::new(opt_lines), body[1]);

    // Description / Help Footer
    let desc_text = if let Some(msg) = &state.status_msg {
        Span::styled(
            msg.clone(),
            Style::default()
                .fg(theme.success.into())
                .add_modifier(Modifier::BOLD),
        )
    } else if let Some(sel_row) = current_rows.get(state.selected) {
        Span::styled(format!("ℹ  {}", sel_row.description), theme.muted())
    } else {
        Span::raw("")
    };

    f.render_widget(
        Paragraph::new(Line::from(desc_text)).alignment(Alignment::Left),
        rows[4],
    );
}

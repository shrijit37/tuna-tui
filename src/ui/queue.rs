//! The Queue view.

use crate::*;

pub(crate) fn render_queue_view(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let inner = area.inner(Margin::new(2, 1));
    if inner.height == 0 {
        return;
    }
    let max = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    // Context header — what's playing from.
    if !app.transport.source_name.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("PLAYING FROM  ", theme.muted()),
            Span::styled(
                truncate(&app.transport.source_name, max.saturating_sub(14)),
                Style::default()
                    .fg(theme.primary.into())
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::raw(""));
    }

    // Now playing — the current track, above the up-next list.
    if let Some(n) = app.playback.now.as_ref() {
        lines.push(Line::from(Span::styled("NOW PLAYING", theme.heading())));
        lines.push(Line::from(vec![
            Span::styled("   ", theme.muted()),
            Span::styled(
                truncate(&n.title, max.saturating_sub(3)),
                Style::default()
                    .fg(theme.text.into())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", n.artist), theme.muted()),
        ]));
        lines.push(Line::raw(""));
    }

    lines.push(Line::from(Span::styled("UP NEXT", theme.heading())));
    lines.push(Line::raw(""));

    let used = lines.len();
    if app.transport.queue.is_empty() {
        lines.push(Line::from(Span::styled("queue is empty", theme.muted())));
    } else {
        let avail_rows = inner.height.saturating_sub(used as u16) as usize;
        let q_len = app.transport.queue.len();
        let selected = app.view.queue_selected.min(q_len.saturating_sub(1));

        // Sticky viewport scrolling: keep selection within visible window
        let offset = if avail_rows == 0 || q_len <= avail_rows {
            0
        } else if selected >= avail_rows {
            (selected + 1)
                .saturating_sub(avail_rows)
                .min(q_len.saturating_sub(avail_rows))
        } else {
            0
        };

        for (i, q) in app
            .transport
            .queue
            .iter()
            .enumerate()
            .skip(offset)
            .take(avail_rows)
        {
            let is_sel = i == selected;
            let (prefix, style) = if is_sel {
                (
                    format!("▶ {:>2}  ", i + 1),
                    Style::default()
                        .fg(theme.primary.into())
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                (
                    format!("  {:>2}  ", i + 1),
                    Style::default().fg(theme.text.into()),
                )
            };
            lines.push(Line::from(vec![
                Span::styled(
                    prefix,
                    if is_sel {
                        Style::default()
                            .fg(theme.primary.into())
                            .add_modifier(Modifier::BOLD)
                    } else {
                        theme.muted()
                    },
                ),
                Span::styled(truncate(q, max.saturating_sub(6)), style),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

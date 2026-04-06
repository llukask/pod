use ratatui::prelude::*;
use ratatui::widgets::*;

use super::text;
use crate::tui::app::InboxState;

pub fn render(frame: &mut Frame, state: &InboxState, area: Rect) {
    let block = Block::bordered().title(" Inbox ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.episodes.is_empty() {
        frame.render_widget(
            Paragraph::new("No new episodes. Press 'r' to sync."),
            inner,
        );
        return;
    }

    // Layout: "[✓] " (4) + podcast (variable) + " │ " (3) + title (variable)
    // + right columns (date + duration + bar ≈ 33) + highlight (2).
    let highlight_width = 2;
    let prefix_width = 4;
    let right_width = 33;
    let separator_width = 3; // " │ "
    let available = (inner.width as usize)
        .saturating_sub(highlight_width + prefix_width + right_width + separator_width);
    // Split available space: ~1/3 for podcast name, ~2/3 for episode title.
    let podcast_width = available / 3;
    let title_width = available.saturating_sub(podcast_width);

    let items: Vec<ListItem> = state
        .episodes
        .iter()
        .enumerate()
        .map(|(i, ep)| {
            let done_marker = if ep.done { "✓" } else { " " };
            let duration = format_duration(ep.audio_duration);

            let progress_pct = if ep.audio_duration > 0 {
                (ep.progress as f64 / ep.audio_duration as f64 * 100.0).min(100.0) as u16
            } else {
                0
            };
            let bar = format_progress_bar(progress_pct, 10);

            let selected = i == state.selected;

            let title_style = if selected {
                Style::default().fg(Color::Yellow).bold()
            } else {
                Style::default()
            };

            let podcast_style = if selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::Blue)
            };

            let dim_style = if selected {
                Style::default().fg(Color::Gray)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let date = &ep.publication_date[..10.min(ep.publication_date.len())];
            let podcast_name = ep.podcast_title.as_deref().unwrap_or("?");

            let title_display = if selected {
                text::scroll(&ep.title, title_width, state.scroll_tick)
            } else {
                text::pad(&ep.title, title_width)
            };

            ListItem::new(Line::from(vec![
                Span::styled(format!("[{}] ", done_marker), Style::default().fg(
                    if ep.done { Color::Green } else { Color::DarkGray }
                )),
                Span::styled(text::pad(podcast_name, podcast_width), podcast_style),
                Span::styled(" │ ", dim_style),
                Span::styled(title_display, title_style),
                Span::styled(format!("  {}  ", date), dim_style),
                Span::styled(format!("{:>6} ", duration), dim_style),
                Span::styled(bar, Style::default().fg(Color::Cyan)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("▸ ");

    let mut list_state = ListState::default().with_selected(Some(state.selected));
    frame.render_stateful_widget(list, inner, &mut list_state);
}

fn format_duration(secs: i32) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    if h > 0 {
        format!("{}h{:02}m", h, m)
    } else {
        format!("{}m", m)
    }
}

fn format_progress_bar(pct: u16, width: usize) -> String {
    let filled = (pct as usize * width / 100).min(width);
    let empty = width - filled;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}

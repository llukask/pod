use ratatui::prelude::*;
use ratatui::widgets::*;

use super::text;
use crate::app::EpisodeListState;
use crate::local_db::DownloadStatus;

pub fn render(frame: &mut Frame, state: &EpisodeListState, area: Rect) {
    let title = format!(" {} — Episodes ", state.podcast_title);
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.loading && state.episodes.is_empty() {
        frame.render_widget(
            Paragraph::new("Loading...").style(Style::default().fg(Color::Yellow)),
            inner,
        );
        return;
    }

    if state.episodes.is_empty() {
        frame.render_widget(
            Paragraph::new("No episodes."),
            inner,
        );
        return;
    }

    // The right-side columns have a fixed width: date (10) + duration
    // (up to 7) + progress bar (12) + spacing (4) ≈ 33 columns.
    // The "[✓] " prefix is 4 columns. The title fills whatever remains.
    let highlight_width = 2; // "▸ "
    let prefix_width = 6;  // "[✓] ● "
    let right_width = 33;  // "  YYYY-MM-DD  XXhXXm [██████████]"
    let title_width = (inner.width as usize)
        .saturating_sub(highlight_width + prefix_width + right_width);

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

            let style = if i == state.selected {
                Style::default().fg(Color::Yellow).bold()
            } else {
                Style::default()
            };

            let date = &ep.publication_date[..10.min(ep.publication_date.len())];
            let selected = i == state.selected;

            // Use a lighter dim color on the highlighted row so it doesn't
            // disappear into the DarkGray background.
            let dim_style = if selected {
                Style::default().fg(Color::Gray)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            // Auto-scroll long titles on the selected row.
            let title_display = if selected {
                text::scroll(&ep.title, title_width, state.scroll_tick)
            } else {
                text::pad(&ep.title, title_width)
            };

            let (dl_icon, dl_color) = match ep.download_status {
                Some(DownloadStatus::Downloading) => ("↓", Color::Yellow),
                Some(DownloadStatus::Complete) => ("●", Color::Magenta),
                Some(DownloadStatus::Failed) => ("!", Color::Red),
                _ => (" ", Color::DarkGray),
            };

            ListItem::new(Line::from(vec![
                Span::styled(format!("[{}] ", done_marker), Style::default().fg(
                    if ep.done { Color::Green } else { Color::DarkGray }
                )),
                Span::styled(format!("{} ", dl_icon), Style::default().fg(dl_color)),
                Span::styled(title_display, style),
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

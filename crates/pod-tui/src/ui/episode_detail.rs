use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::app::EpisodeDetailState;

pub fn render(frame: &mut Frame, state: &EpisodeDetailState, area: Rect) {
    let block = Block::bordered().title(format!(" {} ", state.episode.title));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Build the text content: metadata header + body.
    let mut lines = Vec::new();

    // Metadata.
    let date = &state.episode.publication_date[..10.min(state.episode.publication_date.len())];
    lines.push(Line::from(vec![
        Span::styled("Date: ", Style::default().fg(Color::DarkGray)),
        Span::raw(date),
    ]));

    let duration_secs = state.episode.audio_duration;
    let h = duration_secs / 3600;
    let m = (duration_secs % 3600) / 60;
    let duration_str = if h > 0 {
        format!("{}h {:02}m", h, m)
    } else {
        format!("{}m", m)
    };
    lines.push(Line::from(vec![
        Span::styled("Duration: ", Style::default().fg(Color::DarkGray)),
        Span::raw(duration_str),
    ]));

    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "─".repeat(inner.width as usize),
        Style::default().fg(Color::DarkGray),
    ));
    lines.push(Line::raw(""));

    // Prefer content_encoded (richer), fall back to summary.
    let body = if !state.episode.content_encoded.is_empty() {
        strip_html(&state.episode.content_encoded)
    } else {
        strip_html(&state.episode.summary)
    };

    for line in body.lines() {
        lines.push(Line::raw(line.to_string()));
    }

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .scroll((state.scroll, 0));

    frame.render_widget(paragraph, inner);
}

/// Simple HTML tag stripping. For an MVP this is sufficient; a proper
/// implementation would use the `html2text` crate.
fn strip_html(html: &str) -> String {
    html2text::from_read(html.as_bytes(), 80).unwrap_or_else(|_| html.to_string())
}

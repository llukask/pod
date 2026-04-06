use ratatui::prelude::*;
use ratatui::widgets::*;

use super::text;
use crate::tui::app::PodcastListState;

pub fn render(frame: &mut Frame, state: &PodcastListState, area: Rect) {
    let block = Block::bordered().title(" Podcasts ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.loading && state.podcasts.is_empty() {
        frame.render_widget(
            Paragraph::new("Loading...").style(Style::default().fg(Color::Yellow)),
            inner,
        );
        return;
    }

    if state.podcasts.is_empty() {
        frame.render_widget(
            Paragraph::new("No podcasts. Press 'r' to sync."),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = state
        .podcasts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let date = p
                .last_publication_date
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "—".to_string());

            let style = if i == state.selected {
                Style::default().fg(Color::Yellow).bold()
            } else {
                Style::default()
            };

            ListItem::new(Line::from(vec![
                Span::styled(text::pad(&p.title, 50), style),
                Span::styled(date, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("▸ ");

    let mut list_state = ListState::default().with_selected(Some(state.selected));
    frame.render_stateful_widget(list, inner, &mut list_state);
}


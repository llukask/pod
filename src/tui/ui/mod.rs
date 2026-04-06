mod episode_detail;
mod episode_list;
mod login;
mod podcast_list;
mod status_bar;
mod text;

use ratatui::Frame;

use crate::tui::app::{App, View};

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Reserve the bottom line for the status bar.
    let content_area = ratatui::layout::Rect {
        height: area.height.saturating_sub(1),
        ..area
    };
    let status_area = ratatui::layout::Rect {
        y: area.height.saturating_sub(1),
        height: 1,
        ..area
    };

    match &app.view {
        View::Login(state) => login::render(frame, state, content_area),
        View::PodcastList(state) => podcast_list::render(frame, state, content_area),
        View::EpisodeList(state) => episode_list::render(frame, state, content_area),
        View::EpisodeDetail(state) => episode_detail::render(frame, state, content_area),
    }

    status_bar::render(frame, app, status_area);
}

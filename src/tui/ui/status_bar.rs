use ratatui::prelude::*;
use ratatui::widgets::*;

use super::text;
use crate::tui::app::{App, View};

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    // If we have two lines, use the top one for now-playing and the bottom
    // for view info. Otherwise squeeze into one line.
    if let Some(ref np) = app.now_playing {
        let pause_icon = if np.state.paused { "⏸" } else { "▶" };
        let pos = format_time(np.state.position_secs);
        let dur = format_time(np.state.duration_secs);
        let pct = if np.state.duration_secs > 0 {
            (np.state.position_secs as f64 / np.state.duration_secs as f64 * 100.0) as u16
        } else {
            0
        };
        let bar = progress_bar(pct, 15);

        let sync_indicator = if app.syncing { " [syncing...]" } else { "" };

        let playback_line = format!(
            " {} {} {}/{} {}{} │ Space: pause │ ←/→: seek │ s: stop",
            pause_icon,
            text::truncate(&np.episode_title, 30),
            pos,
            dur,
            bar,
            sync_indicator,
        );

        let playback_bar = Paragraph::new(playback_line)
            .style(Style::default().bg(Color::Rgb(30, 60, 30)).fg(Color::White));
        frame.render_widget(playback_bar, area);
    } else {
        let view_name = match &app.view {
            View::Login(_) => "Login",
            View::PodcastList(_) => "Podcasts",
            View::EpisodeList(_) => "Episodes",
            View::EpisodeDetail(_) => "Detail",
        };

        let help = match &app.view {
            View::Login(_) => "Tab: next field | Enter: submit | Esc: quit",
            View::PodcastList(_) => "j/k: navigate | Enter: select | r: sync | q: quit",
            View::EpisodeList(_) => "j/k: navigate | Enter: detail | p: play | d: done | Esc: back",
            View::EpisodeDetail(_) => "j/k: scroll | Enter: play | Esc: back | q: quit",
        };

        let sync_indicator = if app.syncing { " [syncing...]" } else { "" };

        let status = if let Some(ref msg) = app.status_message {
            format!(" {}{} │ {} │ {}", view_name, sync_indicator, msg, help)
        } else {
            format!(" {}{} │ {}", view_name, sync_indicator, help)
        };

        let bar = Paragraph::new(status)
            .style(Style::default().bg(Color::DarkGray).fg(Color::White));
        frame.render_widget(bar, area);
    }
}

fn format_time(secs: i32) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else {
        format!("{}:{:02}", m, s)
    }
}


fn progress_bar(pct: u16, width: usize) -> String {
    let filled = (pct as usize * width / 100).min(width);
    let empty = width - filled;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}

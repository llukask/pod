//! The root view: which page the window shows, and the shell around it
//! (wireframe D1).

use gpui_kit::component::{ActiveTheme as _, TitleBar, h_flex, v_flex};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, IntoElement, ParentElement as _,
    Render, Result, Styled as _, Window, WindowBounds, WindowOptions, px, size,
};
use pod_protocol::PodcastId;

use crate::shell;

/// The page selected in the sidebar. The list and detail panes show its
/// content; the title bar and player bar stay the same for every route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Inbox,
    Downloads,
    Settings,
    #[expect(
        dead_code,
        reason = "the Podcasts group links here once it lists subscriptions"
    )]
    Podcast(PodcastId),
}

/// Owns navigation state for the main window.
#[derive(Debug)]
pub struct PodApp {
    route: Route,
}

impl PodApp {
    fn new() -> Self {
        Self {
            route: Route::Inbox,
        }
    }

    pub fn navigate(&mut self, route: Route, cx: &mut Context<Self>) {
        if self.route != route {
            self.route = route;
            cx.notify();
        }
    }
}

impl Render for PodApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(shell::title_bar::render(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(shell::sidebar::render(self.route, cx))
                    .child(shell::content::render(self.route, cx)),
            )
            .child(shell::player_bar::render(cx))
    }
}

/// Opens the main window. Requires `gpui_kit::init`.
pub fn open_main_window(cx: &mut App) -> Result<AnyWindowHandle> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1100.), px(720.)),
            cx,
        ))),
        window_min_size: Some(size(px(640.), px(420.))),
        app_id: Some("pod".into()),
        ..TitleBar::window_options()
    };
    let (window, _) = gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| PodApp::new()))?;
    Ok(window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;
    use gpui_kit::test::TestWindowExt as _;

    #[gpui_kit::test]
    fn sidebar_click_switches_the_content_pane(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let window = cx
            .update(open_main_window)
            .expect("the test platform opens windows");

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("page-title").label(), Some("Inbox"));
            assert!(window.find("sync-status").visible());
            assert!(window.find("player-bar").visible());
            assert!(window.find("podcasts-empty").visible());

            window
                .within("sidebar")
                .click(shell::sidebar::page_element_id(Route::Downloads), cx);
            assert_eq!(window.find("page-title").label(), Some("Downloads"));

            window
                .within("sidebar")
                .click(shell::sidebar::page_element_id(Route::Settings), cx);
            assert_eq!(window.find("page-title").label(), Some("Settings"));
        })
        .expect("the window is still open");
    }
}

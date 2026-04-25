// ==============================================================================
// Pod web frontend (Leptos / wasm32)
// ==============================================================================
//
// This binary is compiled to `wasm32-unknown-unknown` by Trunk and mounted into
// the body of `index.html`. It is a 1:1 functional port of the previous
// vanilla-JS SPA at `frontend/index.html` — same API, same routes, same CSS.
//
// Top-level modules:
//   - api     : typed wrappers around `/api/v1/*` and the iTunes search.
//   - state   : globally-shared reactive signals (auth, library, player).
//   - player  : audio-element wiring + progress auto-save.
//   - util    : formatters and small DOM helpers.
//   - views   : per-route view components (auth, dashboard, podcast, …).

use leptos::prelude::*;
use leptos_router::components::{Route, Router, Routes};
use leptos_router::path;

mod api;
mod player;
mod state;
mod util;
mod views;

use crate::state::{provide_app_state, use_app_state};
use crate::views::auth::AuthScreen;
use crate::views::dashboard::DashboardView;
use crate::views::inbox::InboxView;
use crate::views::podcast::PodcastView;
use crate::views::shell::AppShell;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    provide_app_state();
    let state = use_app_state();

    // Auth gate: when there is no token, render the standalone auth screen
    // and skip the router entirely. Once a token exists, the rest of the app
    // (header + routed views + persistent player bar) is mounted.
    view! {
        <Show when=move || state.token.get().is_some()
              fallback=|| view! { <AuthScreen /> }>
            <Router>
                <AppShell>
                    <Routes fallback=|| view! { <p>"Not found"</p> }>
                        <Route path=path!("/") view=DashboardView />
                        <Route path=path!("/inbox") view=InboxView />
                        <Route path=path!("/podcast/:id") view=PodcastView />
                    </Routes>
                </AppShell>
            </Router>
        </Show>
    }
}

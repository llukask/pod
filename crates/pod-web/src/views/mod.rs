// Per-route view components. Each submodule renders one screen and is
// (almost) free of cross-screen knowledge — shared state flows through
// `crate::state::AppState`.

pub mod auth;
pub mod dashboard;
pub mod episode_modal;
pub mod inbox;
pub mod podcast;
pub mod shared;
pub mod shell;

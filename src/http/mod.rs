use std::sync::Arc;

use axum::extract::FromRef;
use axum_extra::extract::cookie::Key;

use reqwest::Client as ReqwestClient;

use crate::{app::App, db::Db};

pub mod web;

pub mod auth;
pub mod errors;

#[derive(Clone)]
pub struct AppState {
    pub app: Arc<App>,
    pub db: Arc<Db>,
    pub http: ReqwestClient,
    pub key: Key,

    pub cookie_domain: String,
    pub base_url: String,
}

impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Self {
        state.key.clone()
    }
}

use std::sync::Arc;

use reqwest::Client as ReqwestClient;

use crate::{app::App, db::Db};

pub mod api;

pub mod auth;
pub mod errors;

#[derive(Clone)]
pub struct AppState {
    pub app: Arc<App>,
    pub db: Arc<Db>,
    pub http: ReqwestClient,

    pub allow_registration: bool,
}

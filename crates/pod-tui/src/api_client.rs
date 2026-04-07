use anyhow::Context;
use reqwest::header;
use serde::Deserialize;

use pod_model::{
    Episode, PodcastWithEpisodeStats, ProgressState, ProgressSyncResponse, SyncResponse,
};

pub struct ApiClient {
    base_url: String,
    token: Option<String>,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct AuthResponse {
    token: String,
    expires_at: String,
}

#[derive(Deserialize)]
struct EpisodePage {
    items: Vec<EpisodeWithProgressApi>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct EpisodeWithProgressApi {
    episode: Episode,
    progress: Option<i32>,
    done: bool,
}

#[derive(Deserialize)]
struct SyncHeadResponse {
    since: String,
}

impl ApiClient {
    pub fn new(base_url: &str, token: Option<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            http: reqwest::Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn auth_request(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref token) = self.token {
            req.header(header::AUTHORIZATION, format!("Bearer {}", token))
        } else {
            req
        }
    }

    /// Check the response status and return a detailed error including the
    /// response body if the request failed.
    async fn check(resp: reqwest::Response, endpoint: &str) -> anyhow::Result<reqwest::Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let url = resp.url().to_string();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("{} failed ({}): {} — {}", endpoint, status, url, body);
    }

    // ==========================================================================
    // Auth
    // ==========================================================================

    pub async fn login(
        &self,
        username: &str,
        password: &str,
    ) -> anyhow::Result<(String, String)> {
        let resp = self
            .http
            .post(self.url("/api/v1/auth/login"))
            .json(&serde_json::json!({
                "username": username,
                "password": password,
            }))
            .send()
            .await
            .context("send login request")?;

        let resp = Self::check(resp, "login").await?;
        let auth: AuthResponse = resp.json().await.context("parse login response")?;
        Ok((auth.token, auth.expires_at))
    }

    // ==========================================================================
    // Podcasts & Episodes
    // ==========================================================================

    pub async fn list_podcasts(&self) -> anyhow::Result<Vec<PodcastWithEpisodeStats>> {
        let resp = self
            .auth_request(self.http.get(self.url("/api/v1/podcasts")))
            .send()
            .await
            .context("send list_podcasts request")?;

        let resp = Self::check(resp, "list_podcasts").await?;
        Ok(resp.json().await.context("parse list_podcasts response")?)
    }

    /// Fetch one page of episodes for a podcast.
    pub async fn list_episodes(
        &self,
        podcast_id: &str,
        per_page: i32,
        page_token: Option<&str>,
    ) -> anyhow::Result<(Vec<(Episode, Option<i32>, bool)>, Option<String>)> {
        let mut req = self
            .http
            .get(self.url(&format!("/api/v1/podcasts/{}/episodes", podcast_id)))
            .query(&[("per_page", &per_page.to_string())]);
        if let Some(token) = page_token {
            req = req.query(&[("page_token", token)]);
        }

        let resp = self
            .auth_request(req)
            .send()
            .await
            .context("send list_episodes request")?;

        let resp = Self::check(resp, "list_episodes").await?;
        let page: EpisodePage = resp.json().await.context("parse list_episodes response")?;
        let items = page
            .items
            .into_iter()
            .map(|ewp| (ewp.episode, ewp.progress, ewp.done))
            .collect();
        Ok((items, page.next_page_token))
    }

    pub async fn report_progress(
        &self,
        episode_id: &str,
        progress: i32,
        done: bool,
    ) -> anyhow::Result<ProgressState> {
        let resp = self
            .auth_request(
                self.http
                    .post(self.url(&format!("/api/v1/episodes/{}/progress", episode_id)))
                    .json(&serde_json::json!({
                        "progress": progress,
                        "done": done,
                    })),
            )
            .send()
            .await
            .context("send report_progress request")?;

        let resp = Self::check(resp, "report_progress").await?;
        Ok(resp.json().await.context("parse report_progress response")?)
    }

    // ==========================================================================
    // Sync
    // ==========================================================================

    pub async fn sync_head(&self) -> anyhow::Result<String> {
        let resp = self
            .auth_request(self.http.get(self.url("/api/v1/sync/head")))
            .send()
            .await
            .context("send sync_head request")?;

        let resp = Self::check(resp, "sync_head").await?;
        let head: SyncHeadResponse = resp.json().await.context("parse sync_head response")?;
        Ok(head.since)
    }

    pub async fn sync_changes(
        &self,
        since: &str,
        limit: i64,
    ) -> anyhow::Result<SyncResponse> {
        let resp = self
            .auth_request(
                self.http
                    .get(self.url("/api/v1/sync/changes"))
                    .query(&[("since", since), ("limit", &limit.to_string())]),
            )
            .send()
            .await
            .context("send sync_changes request")?;

        let resp = Self::check(resp, "sync_changes").await?;
        Ok(resp.json().await.context("parse sync_changes response")?)
    }

    pub async fn sync_progress(
        &self,
        since: Option<&str>,
    ) -> anyhow::Result<ProgressSyncResponse> {
        let mut req = self.http.get(self.url("/api/v1/sync/progress"));
        if let Some(since) = since {
            req = req.query(&[("since", since)]);
        }

        let resp = self
            .auth_request(req)
            .send()
            .await
            .context("send sync_progress request")?;

        let resp = Self::check(resp, "sync_progress").await?;
        Ok(resp.json().await.context("parse sync_progress response")?)
    }
}

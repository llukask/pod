//! A device's Connect event stream (spec 14.2) read event by event, for the
//! Connect tests.

// Each test binary uses a different subset.
#![allow(dead_code)]

use std::time::Duration;

use axum::body::{Body, BodyDataStream};
use axum::http::{Request, StatusCode, header};
use futures_util::StreamExt;
use serde_json::Value;

use crate::auth_app::TestApp;

/// How long a test waits for an event before it fails.
const WAIT: Duration = Duration::from_secs(5);

pub struct Events {
    body: BodyDataStream,
    buffer: String,
}

impl Events {
    /// Opens the stream for the token's device. Panics unless the server
    /// answers with an event stream.
    pub async fn open(app: &TestApp, token: &str, kind: &str) -> Events {
        let response = app
            .raw(
                Request::get(format!("/v1/connect/events?kind={kind}"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        assert_eq!(response.headers()["x-accel-buffering"], "no");
        Events {
            body: response.into_body().into_data_stream(),
            buffer: String::new(),
        }
    }

    /// The next event's JSON, skipping comments. Panics after [`WAIT`].
    pub async fn next(&mut self) -> Value {
        tokio::time::timeout(WAIT, self.read())
            .await
            .expect("an event arrives in time")
            .expect("the stream stays open")
    }

    /// The next event, or `None` when the stream ended.
    pub async fn read(&mut self) -> Option<Value> {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let frame: String = self.buffer.drain(..end + 2).collect();
                let data: Vec<&str> = frame
                    .lines()
                    .filter_map(|line| line.strip_prefix("data:"))
                    .map(str::trim_start)
                    .collect();
                if data.is_empty() {
                    continue;
                }
                return Some(serde_json::from_str(&data.join("\n")).expect("events are JSON"));
            }
            let chunk = self.body.next().await?.expect("the body streams");
            self.buffer
                .push_str(std::str::from_utf8(&chunk).expect("events are UTF-8"));
        }
    }

    /// Skips events until one of `kind` arrives.
    pub async fn next_of(&mut self, kind: &str) -> Value {
        loop {
            let event = self.next().await;
            if event["type"] == kind {
                return event;
            }
        }
    }

    /// Skips `state` events until one satisfies `matches`.
    pub async fn state_where(&mut self, matches: impl Fn(&Value) -> bool) -> Value {
        loop {
            let event = self.next_of("state").await;
            if matches(&event) {
                return event;
            }
        }
    }

    /// Whether the stream ended within `wait`, ignoring events.
    pub async fn ends_within(&mut self, wait: Duration) -> bool {
        tokio::time::timeout(wait, async { while self.read().await.is_some() {} })
            .await
            .is_ok()
    }

    /// Asserts nothing but `state` arrives within `wait`, and returns the
    /// last of those.
    pub async fn quiet_for(&mut self, wait: Duration) -> Option<Value> {
        let mut last = None;
        let deadline = tokio::time::Instant::now() + wait;
        while let Ok(event) = tokio::time::timeout_at(deadline, self.read()).await {
            let event = event.expect("the stream stays open");
            assert_eq!(event["type"], "state", "unexpected event {event}");
            last = Some(event);
        }
        last
    }
}

use thiserror::Error;

#[derive(Debug, Clone)]
pub struct FeedRef {
    pub url: String,
    pub typ: String,
}

#[derive(Debug, Error)]
pub enum GetFeedError {
    #[error("reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("feed_rs error: {0}")]
    FeedRs(#[from] feed_rs::parser::ParseFeedError),
}

pub async fn get_feed(
    c: &reqwest::Client,
    url: &str,
) -> Result<feed_rs::model::Feed, GetFeedError> {
    let req = c.get(url).build();
    let res = c.execute(req?).await?;
    let text = res.text().await?;
    let parsed_feed = feed_rs::parser::parse(text.as_bytes())?;
    Ok(parsed_feed)
}

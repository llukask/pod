//! Episode chapters (spec 17).
//!
//! Every source (Podlove chapters in the feed, a `podcast:chapters` JSON
//! file, ID3 `CHAP` frames) yields [`RawChapter`]s, and [`normalize`] turns
//! them into the one list clients get. [`lookup`] tries the sources in the
//! spec's order and keeps what it fetched in `chapter_cache`.

pub mod id3;
mod json;

use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr, bail};
use pod_protocol::{Chapter, EpisodeId, Timestamp, UserId};
use reqwest::header::{ACCEPT_ENCODING, HeaderMap, HeaderValue, RANGE};
use sqlx::PgPool;
use sqlx::types::Json;
use url::Url;

use crate::db::SqlTimestamp;
use crate::fetch::{BodyLimit, Fetcher};

/// Chapters kept per episode (spec 17.1).
pub const MAX_CHAPTERS: usize = 500;

/// Largest JSON chapters file read (spec 17.2).
const MAX_JSON_BYTES: usize = 1024 * 1024;

/// How long a fetched empty result stands before the sources are asked again
/// (spec 17.2). A list with chapters stands until ingest rewrites the episode.
const EMPTY_RESULT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// What [`lookup`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// The episode doesn't exist, is deleted, or the user can't read its
    /// podcast.
    NotFound,
    /// The episode's chapters, maybe none.
    Chapters(Vec<Chapter>),
    /// No source yielded chapters and at least one failed, so trying again
    /// later may find some.
    Unavailable,
}

/// The chapters of `episode_id` for `user_id` at `now` (spec 17.2).
///
/// A source that fails is logged and skipped; only database errors are
/// `Err`. The caller bounds the time this takes, since a slow host can
/// spend the fetch timeout on each of two sources.
pub async fn lookup(
    pool: &PgPool,
    fetcher: &Fetcher,
    user_id: UserId,
    episode_id: EpisodeId,
    now: Timestamp,
) -> Result<Lookup> {
    // A podcast is readable when the user is subscribed to it without a
    // redirect (spec 4.5); the episode's foreign key guarantees its row.
    let Some(row) = sqlx::query!(
        r#"select e.feed_chapters as "feed_chapters: Json<Vec<Chapter>>",
                  e.chapters_url, e.audio_url,
                  c.chapters as "cached?: Json<Vec<Chapter>>",
                  c.fetched_at as "fetched_at?: SqlTimestamp"
           from episode e
           join subscription s on s.podcast_id = e.podcast_id
           left join chapter_cache c on c.episode_id = e.id
           where e.id = $1 and not e.deleted
             and s.user_id = $2 and s.subscribed and s.replaced_by is null"#,
        episode_id.as_uuid(),
        user_id.as_uuid(),
    )
    .fetch_optional(pool)
    .await
    .context("read the episode's chapter sources")?
    else {
        return Ok(Lookup::NotFound);
    };

    if let Some(Json(chapters)) = row
        .feed_chapters
        .filter(|Json(chapters)| !chapters.is_empty())
    {
        return Ok(Lookup::Chapters(chapters));
    }
    if let (Some(Json(cached)), Some(fetched_at)) = (row.cached, row.fetched_at) {
        let expires_ms = fetched_at.0.as_epoch_ms() + EMPTY_RESULT_TTL.as_millis() as i64;
        if !cached.is_empty() || now.as_epoch_ms() < expires_ms {
            return Ok(Lookup::Chapters(cached));
        }
    }

    // -------------------------------------------------------------------------
    // Fetch: the JSON file first, then the audio file's ID3 tag.
    // -------------------------------------------------------------------------

    let mut failed = false;
    let mut found = Vec::new();
    if let Some(url) = &row.chapters_url {
        match from_json(fetcher, url).await {
            Ok(chapters) => found = chapters,
            Err(error) => {
                tracing::info!(%episode_id, error = %format!("{error:#}"), "can't read the JSON chapters");
                failed = true;
            }
        }
    }
    if found.is_empty() {
        match from_id3(fetcher, &row.audio_url).await {
            Ok(chapters) => found = chapters,
            Err(error) => {
                tracing::info!(%episode_id, error = %format!("{error:#}"), "can't read the audio file's chapters");
                failed = true;
            }
        }
    }

    // A failure may have hidden chapters, so only a clean result is kept.
    if !failed {
        sqlx::query!(
            "insert into chapter_cache (episode_id, chapters, fetched_at)
             values ($1, $2, $3)
             on conflict (episode_id) do update set
                 chapters = excluded.chapters, fetched_at = excluded.fetched_at",
            episode_id.as_uuid(),
            Json(&found) as _,
            SqlTimestamp(now) as _,
        )
        .execute(pool)
        .await
        .context("cache the fetched chapters")?;
    }
    Ok(if found.is_empty() && failed {
        Lookup::Unavailable
    } else {
        Lookup::Chapters(found)
    })
}

/// The chapters of the JSON file at `url`.
async fn from_json(fetcher: &Fetcher, url: &str) -> Result<Vec<Chapter>> {
    let url = Url::parse(url).context("parse the chapters URL")?;
    let response = fetcher
        .get_with_limit(&url, HeaderMap::new(), BodyLimit::Reject(MAX_JSON_BYTES))
        .await
        .context("fetch the chapters file")?;
    if !response.status.is_success() {
        bail!("the chapters file answered HTTP {}", response.status);
    }
    let raw = json::parse(&response.body).context("parse the chapters file")?;
    Ok(normalize(raw))
}

/// The chapters in the ID3 tag at the start of the audio file at `url`.
///
/// Two small range requests instead of one large one: analytics that follow
/// the IAB guidelines don't count either as a download (spec 17.2). A host
/// that ignores `Range` answers 200 with the whole file, of which only the
/// needed bytes are read.
async fn from_id3(fetcher: &Fetcher, url: &str) -> Result<Vec<Chapter>> {
    let url = Url::parse(url).context("parse the audio URL")?;
    let header = read_prefix(fetcher, &url, id3::HEADER_BYTES)
        .await
        .context("read the ID3 header")?;
    let Some(size) = id3::tag_size(&header) else {
        return Ok(Vec::new());
    };
    if size > id3::MAX_TAG_BYTES {
        return Ok(Vec::new());
    }
    let tag = read_prefix(fetcher, &url, size)
        .await
        .context("read the ID3 tag")?;
    if tag.len() < size {
        bail!("the audio file ended inside its ID3 tag");
    }
    Ok(normalize(id3::chapters(&tag)))
}

/// The first `len` bytes of the file at `url`, or fewer if it is shorter.
async fn read_prefix(fetcher: &Fetcher, url: &Url, len: usize) -> Result<bytes::Bytes> {
    let mut headers = HeaderMap::new();
    let range = format!("bytes=0-{}", len - 1);
    headers.insert(
        RANGE,
        HeaderValue::from_str(&range).expect("a byte range is a valid header value"),
    );
    // Byte offsets must count the file itself, not a compressed transfer.
    headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    let response = fetcher
        .get_with_limit(url, headers, BodyLimit::Truncate(len))
        .await
        .context("fetch the start of the audio file")?;
    if !response.status.is_success() {
        bail!("the audio file answered HTTP {}", response.status);
    }
    Ok(response.body)
}

/// A chapter as a source wrote it, before [`normalize`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawChapter {
    pub start_ms: Option<i64>,
    pub title: Option<String>,
    pub url: Option<String>,
    /// False for a JSON chapter marked `"toc": false`.
    pub toc: bool,
}

/// Applies spec 17.1: drops chapters without a usable start and those out of
/// the table of contents, sorts by start, keeps the first of each start, caps
/// the count, names untitled chapters and keeps only `http(s)` links.
pub fn normalize(raw: impl IntoIterator<Item = RawChapter>) -> Vec<Chapter> {
    let mut kept: Vec<(i64, RawChapter)> = raw
        .into_iter()
        .filter(|chapter| chapter.toc)
        .filter_map(|chapter| Some((chapter.start_ms.filter(|&ms| ms >= 0)?, chapter)))
        .collect();
    // Stable, so of two chapters with one start the one written first stays.
    kept.sort_by_key(|(start_ms, _)| *start_ms);
    kept.dedup_by_key(|(start_ms, _)| *start_ms);
    kept.truncate(MAX_CHAPTERS);
    kept.into_iter()
        .enumerate()
        .map(|(index, (start_ms, chapter))| Chapter {
            start_ms,
            title: chapter
                .title
                .map(|title| title.trim().to_owned())
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| format!("Chapter {}", index + 1)),
            url: chapter.url.as_deref().and_then(crate::feed::http_url),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(start_ms: Option<i64>, title: &str) -> RawChapter {
        RawChapter {
            start_ms,
            title: Some(title.to_owned()),
            url: None,
            toc: true,
        }
    }

    #[test]
    fn sorts_dedups_and_names_chapters() {
        let chapters = normalize([
            raw(Some(60_000), "Second"),
            raw(Some(0), "  "),
            raw(Some(60_000), "Same start"),
            raw(None, "No start"),
            raw(Some(-1), "Negative"),
            RawChapter {
                toc: false,
                ..raw(Some(30_000), "Artwork change")
            },
            RawChapter {
                url: Some("javascript:alert(1)".to_owned()),
                ..raw(Some(90_000), "Third")
            },
        ]);
        let summary: Vec<_> = chapters
            .iter()
            .map(|chapter| {
                (
                    chapter.start_ms,
                    chapter.title.as_str(),
                    chapter.url.is_some(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (0, "Chapter 1", false),
                (60_000, "Second", false),
                (90_000, "Third", false)
            ]
        );
    }

    #[test]
    fn keeps_at_most_five_hundred() {
        let chapters = normalize((0..600).map(|n| raw(Some(n * 1000), "x")));
        assert_eq!(chapters.len(), MAX_CHAPTERS);
        assert_eq!(
            chapters.last().map(|chapter| chapter.start_ms),
            Some(499_000)
        );
    }
}

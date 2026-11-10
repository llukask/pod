//! The Podcasting 2.0 JSON chapters format that `<podcast:chapters>` points
//! to (spec 17.2): `{"version": "1.2.0", "chapters": [{"startTime": 0,
//! "title": "Intro", "url": "...", "toc": false}]}`.

use serde::Deserialize;

use super::RawChapter;

#[derive(Debug, Deserialize)]
struct File {
    chapters: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    start_time: Option<f64>,
    title: Option<String>,
    url: Option<String>,
    toc: Option<bool>,
}

/// The chapters of a JSON chapters file, or an error when it isn't one.
pub fn parse(body: &[u8]) -> Result<Vec<RawChapter>, serde_json::Error> {
    let file: File = serde_json::from_slice(body)?;
    Ok(file
        .chapters
        .into_iter()
        .map(|entry| RawChapter {
            // `startTime` is seconds, often fractional. A start too large for
            // milliseconds is no start at all.
            start_ms: entry
                .start_time
                .filter(|seconds| seconds.is_finite())
                .map(|seconds| (seconds * 1000.0).round())
                .filter(|ms| (i64::MIN as f64..i64::MAX as f64).contains(ms))
                .map(|ms| ms as i64),
            title: entry.title,
            url: entry.url,
            toc: entry.toc.unwrap_or(true),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_start_title_url_and_toc() {
        let chapters = parse(
            br#"{"version": "1.2.0", "chapters": [
                {"startTime": 0, "title": "Intro"},
                {"startTime": 754.5, "title": "Ads", "toc": false, "img": "https://x/y.jpg"},
                {"startTime": 1e300, "title": "Too far"},
                {"title": "No start", "url": "https://example.com"}
            ]}"#,
        )
        .expect("a valid chapters file");
        assert_eq!(
            chapters,
            [
                RawChapter {
                    start_ms: Some(0),
                    title: Some("Intro".to_owned()),
                    url: None,
                    toc: true,
                },
                RawChapter {
                    start_ms: Some(754_500),
                    title: Some("Ads".to_owned()),
                    url: None,
                    toc: false,
                },
                RawChapter {
                    start_ms: None,
                    title: Some("Too far".to_owned()),
                    url: None,
                    toc: true,
                },
                RawChapter {
                    start_ms: None,
                    title: Some("No start".to_owned()),
                    url: Some("https://example.com".to_owned()),
                    toc: true,
                },
            ]
        );
    }

    #[test]
    fn a_body_that_is_not_a_chapters_file_is_an_error() {
        assert!(parse(b"<html>Not found</html>").is_err());
        assert!(parse(br#"{"version": "1.2.0"}"#).is_err());
    }
}

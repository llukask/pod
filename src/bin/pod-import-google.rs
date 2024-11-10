use anyhow::Result;
use pod::{
    db::Db,
    feed::{get_feed, FeedRef},
    model::{Episode, Podcast},
};
use sqlx::postgres::PgPoolOptions;
use std::{
    fs::File,
    io::{BufReader, Read},
    sync::Arc,
};

use xml::{reader::XmlEvent, EventReader};

#[derive(Debug)]
struct GooglePodcastsExport {
    feeds: Vec<FeedRef>,
}

fn read_google_podcast_export<R: Read>(r: R) -> Result<GooglePodcastsExport> {
    let parser = EventReader::new(r);

    let mut in_body = false;
    let mut in_feeds_outline = false;
    let mut depth = 0;

    let mut feeds = Vec::new();

    for e in parser {
        match e {
            Ok(XmlEvent::StartElement {
                name, attributes, ..
            }) => {
                if !in_body && name.local_name == "body" {
                    in_body = true;
                } else if in_body && name.local_name == "outline" {
                    if !in_feeds_outline {
                        if let Some(text) = attributes.iter().find(|a| a.name.local_name == "text")
                        {
                            if text.value == "feeds" {
                                in_feeds_outline = true;
                            }
                        }
                    } else {
                        let mut url = None;
                        let mut typ = None;

                        for a in attributes {
                            match a.name.local_name.as_str() {
                                "xmlUrl" => url = Some(a.value),
                                "type" => typ = Some(a.value),
                                _ => {}
                            }
                        }

                        feeds.push(FeedRef {
                            url: url.unwrap(),
                            typ: typ.unwrap(),
                        });
                    }
                }
                depth += 1;
            }
            Ok(XmlEvent::EndElement { name }) => {
                if in_body && name.local_name == "body" {
                    in_body = false;
                } else if in_feeds_outline && depth < 4 {
                    in_feeds_outline = false;
                }
                depth -= 1;
            }
            Err(e) => {
                return Err(anyhow::anyhow!("could not parse XML: {}", e));
            }

            _ => {}
        }
    }

    let export = GooglePodcastsExport { feeds };

    Ok(export)
}

#[tokio::main]
async fn main() -> Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect("postgres://pod:pod@localhost/pod")
        .await?;

    let db = Db::init(pool).await?;

    let client = Arc::new(
        reqwest::ClientBuilder::new()
            .danger_accept_invalid_certs(true)
            .build()?,
    );

    let file = File::open("google-podcasts-subscriptions.opml")?;
    let file = BufReader::new(file);

    let export = read_google_podcast_export(file)?;
    for feed_ref in export.feeds.iter() {
        eprintln!("fetching feed: {}", feed_ref.url);

        let feed = get_feed(&client, &feed_ref.url).await?;

        let title = feed.title.as_ref().unwrap().content.clone();
        let id = feed.id.clone();

        let now = chrono::Utc::now();
        let logo_link = feed.logo.as_ref().map(|l| l.uri.clone());
        let icon_link = feed.icon.as_ref().map(|l| l.uri.clone());

        let image_link = logo_link
            .or(icon_link)
            .expect("image or logo link is required");

        let podcast = Podcast {
            id: id.clone(),
            title,
            description: feed
                .description
                .map(|d| d.content.clone())
                .unwrap_or_else(String::new),
            image_link,
            feed_url: feed_ref.url.clone(),
            feed_type: feed_ref.typ.clone(),
            created_at: now,
            last_updated: now,
        };
        db.insert_podcast(&podcast).await?;

        for entry in feed.entries {
            let media = entry.media.iter().find(|m| {
                m.content.iter().any(|c| {
                    c.content_type
                        .as_ref()
                        .map(|mt| mt.essence_str() == "audio/mpeg")
                        .unwrap_or(false)
                })
            });

            if let Some(media) = media {
                let audio_content = media
                    .content
                    .iter()
                    .find(|c| {
                        c.content_type
                            .as_ref()
                            .map(|mt| mt.essence_str() == "audio/mpeg")
                            .unwrap_or(false)
                    })
                    .expect("audio content is required");

                let duration = media
                    .duration
                    .or(audio_content.duration)
                    .map(|d| d.as_secs().try_into().unwrap())
                    .unwrap_or(0);

                let thumbnail_url = media.thumbnails.first().map(|t| t.image.uri.clone());

                let summary_text = entry.summary.as_ref();

                let episode = Episode {
                    id: entry.id.clone(),
                    podcast_id: id.clone(),

                    title: entry.title.as_ref().unwrap().content.clone(),
                    summary: entry
                        .summary
                        .as_ref()
                        .map(|s| s.content.clone())
                        .unwrap_or_else(String::new),
                    summary_type: summary_text
                        .map(|s| s.content_type.essence_str().to_string())
                        .unwrap_or_else(String::new),

                    publication_date: entry.published.unwrap_or(now),

                    audio_url: audio_content
                        .url
                        .as_ref()
                        .expect("audio url is required")
                        .as_str()
                        .to_string(),
                    audio_type: audio_content
                        .content_type
                        .as_ref()
                        .expect("audio type is required")
                        .essence_str()
                        .to_string(),
                    audio_duration: duration,

                    thumbnail_url,

                    created_at: now,
                    last_updated: now,
                };

                db.insert_episode(episode).await?;
            } else {
                eprintln!("no media content found for episode: {}", entry.id);
                eprintln!("{:#?}", entry);
            }
        }
    }

    Ok(())
}

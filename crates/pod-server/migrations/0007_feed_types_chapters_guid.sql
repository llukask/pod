-- Show and episode types (spec 16), chapter sources and their cache
-- (spec 17), and the show guid that finds duplicate feeds (spec 18).

alter table podcast
    add column show_type    text not null default 'episodic'
                            check (show_type in ('episodic', 'serial')),
    add column podcast_guid uuid;

create index on podcast (podcast_guid) where podcast_guid is not null;

alter table episode
    add column episode_type  text not null default 'full'
                             check (episode_type in ('full', 'trailer', 'bonus')),
    add column chapters_url  text,
    add column feed_chapters jsonb;

create table chapter_cache (
    episode_id  uuid primary key references episode(id),
    chapters    jsonb not null,
    fetched_at  timestamptz not null
);

-- The new fields enter the content hashes only when they differ from their
-- defaults (spec 3.3), so most rows keep their hashes. Feeds that carry the
-- new tags still need a full fetch and ingest to fill them in, which an
-- unchanged feed's 304 or matching feed hash would skip.
update feed_poll set etag = null, last_modified = null, content_hash = null;

-- The local schema: spec 5.1 (sync mirror, outbox, account) and 7.1
-- (device-local download state), verbatim. Timestamps are epoch ms.

create table podcast (
    id text primary key, feed_url text not null, status text not null,
    status_message text, title text, author text, description text, link text,
    artwork_url text, language text,
    version integer not null,          -- 0 for a local placeholder
    local_title text,                  -- from directory search, shown while pending
    local_artwork_url text
);

create table episode (
    id text primary key, podcast_id text not null, title text not null,
    summary_html text not null, content_html text not null, link text,
    published_at integer not null,     -- epoch ms
    duration_ms integer, audio_url text not null, audio_type text not null,
    audio_bytes integer, artwork_url text, season integer, number integer,
    version integer not null
);
create index episode_podcast_pub on episode (podcast_id, published_at desc);
create index episode_pub on episode (published_at desc);

create table subscription (
    podcast_id text primary key, feed_url text not null,
    subscribed integer not null, subscribed_at integer not null, subscribed_dev text not null,
    replaced_by text
);

create table episode_state (
    episode_id text primary key, podcast_id text not null,
    position_ms integer not null default 0, position_at integer, position_dev text,
    played integer not null default 0, played_at integer, played_dev text,
    archived integer not null default 0, archived_at integer, archived_dev text
);
create index episode_state_played on episode_state (played, archived);

create table setting (
    key text primary key, value text not null,  -- JSON
    value_at integer not null, value_dev text not null
);

create table sync_scope (
    scope text primary key,            -- 'user' or 'podcast:<id>'
    version integer not null,
    complete integer not null default 0
);

create table outbox (
    mutation_id text primary key,      -- local timestamps are epoch ms everywhere
    coalesce_key text not null,        -- e.g. 'position:<episode_id>'
    payload text not null,             -- JSON mutation
    created_at integer not null,
    attempts integer not null default 0
);
create index outbox_key on outbox (coalesce_key);

create table account (
    id integer primary key check (id = 1),
    server_url text not null, user_id text not null, username text not null,
    device_id text not null, token text not null
);

create table podcast_device (
    podcast_id        text primary key,
    notify            integer not null default 0,
    auto_keep         integer not null default 0,   -- keep newest N unplayed, 0 = off
    auto_on_metered   integer not null default 0
);

create table download_wish (
    episode_id text not null,
    source     text not null check (source in ('pin', 'auto')),
    created_at integer not null,
    metered_ok integer not null default 0,  -- pin only: "download on mobile data anyway"
    primary key (episode_id, source)
);

create table download (
    episode_id     text primary key,
    state          text not null,       -- see 7.2
    audio_url      text not null,       -- URL this file was fetched from
    path           text not null,       -- relative to the downloads root
    bytes_total    integer,
    bytes_done     integer not null default 0,   -- durable checkpoint, see 7.3
    validator      text,                -- ETag or Last-Modified for If-Range
    attempts       integer not null default 0,
    next_attempt_at integer,
    last_error     text,
    wait_reason    text,                -- why a queued row isn't running: connection, wifi, space, budget
    completed_at   integer,
    unwanted_since integer,             -- set when no wish covers the row, cleared when one does
    run_id         text                 -- id of the transfer that owns the row and its .part file
);

-- Sizes learned from Content-Length / Content-Range. Kept apart from
-- `download` so cancelling or evicting a row doesn't forget them, and
-- budget fitting doesn't fall back to a smaller guess and retry the
-- same download on the next pass.
create table known_size (
    episode_id text primary key,
    bytes      integer not null
);

create table device_setting (key text primary key, value text not null);

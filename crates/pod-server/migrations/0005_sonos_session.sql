-- Playback on Sonos (spec 13.3): one row per group that plays a user's
-- episode, so a server restart resumes tracking it.
create table sonos_session (
    player_id      text primary key,     -- the coordinator's UUID
    user_id        uuid not null references users(id) on delete cascade,
    episode_id     uuid not null,
    audio_url      text not null,        -- the URI sent to the speaker
    device_id      uuid not null,        -- *_dev of this session's writes, UUIDv7 per session
    duration_ms    bigint,               -- from the episode row at start
    position_ms    bigint not null,      -- last position observed while playing or paused
    playing        boolean not null,     -- the last observation was PLAYING
    played_written boolean not null default false,
    started_at     timestamptz not null default now()
);

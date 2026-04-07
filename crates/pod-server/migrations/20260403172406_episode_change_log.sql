-- Monotonic change log for delta-based episode sync.
-- Each row records that an episode was inserted or updated; clients
-- pull changes since their last-seen seq to stay up to date.

CREATE TABLE episode_change (
    seq BIGSERIAL PRIMARY KEY,
    podcast_id TEXT NOT NULL REFERENCES podcast(id),
    episode_id TEXT NOT NULL REFERENCES episode(id),
    op TEXT NOT NULL CHECK (op IN ('upsert', 'delete')),
    changed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX episode_change_seq_idx
    ON episode_change(seq);

CREATE INDEX episode_change_podcast_seq_idx
    ON episode_change(podcast_id, seq);

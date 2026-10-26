-- The artwork cache (spec 7.5): one row per image file under
-- `<data_dir>/artwork`, written once the file is complete. Views join it on
-- `podcast.artwork_url` and `episode.artwork_url`, so a live query learns
-- when an image arrives or is evicted.

create table artwork (
    url       text primary key,
    file      text not null,      -- name under the artwork root, from a hash of `url`
    bytes     integer not null,
    last_used integer not null    -- epoch ms of the last pass that wanted it, for LRU eviction
);

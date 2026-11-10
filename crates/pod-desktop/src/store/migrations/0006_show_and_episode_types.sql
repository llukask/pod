-- Show and episode types (spec 16): `itunes:type` on the podcast and
-- `itunes:episodeType` on the episode, with the wire defaults.
--
-- Rows pulled before this migration dropped both fields, and the server
-- sends a row again only when it changes. Every podcast scope therefore
-- restarts at version 0, so the next sync pulls each show again and fills
-- the columns in (spec 5.1). The user scope keeps its cursor.

alter table podcast add column show_type text not null default 'episodic';
alter table episode add column episode_type text not null default 'full';

update sync_scope set version = 0, complete = 0 where scope like 'podcast:%';

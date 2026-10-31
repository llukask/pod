-- Spec 13.3, "Starting": a session plays from 0:00 and seeks once the group
-- plays, because a seek sent while a slow host's stream still loads makes the
-- speaker give up. This is the pending start position; null once it landed.
alter table sonos_session add column seek_to_ms bigint;

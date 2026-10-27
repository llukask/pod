-- A failing podcast carries why and since when (spec 4.2, 5.5), in place of
-- the free-text `status_message`. `feed_error` holds the wire `FeedError`
-- object as JSON text, `failing_since` an epoch ms.
--
-- Rows that are failing today were pulled under the old shape and have no
-- reason to show. Their scopes restart at version 0, so the next sync pulls
-- the podcast row again with its reason; a failing feed has few episodes to
-- re-pull with it.

alter table podcast drop column status_message;
alter table podcast add column failing_since integer;
alter table podcast add column feed_error text;

update sync_scope set version = 0, complete = 0
where scope in (select 'podcast:' || id from podcast where status = 'failing');

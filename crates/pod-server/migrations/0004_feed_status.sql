-- A failing podcast records why and since when, in place of the free-text
-- `status_message` (spec 2.2, 3.2).
--
-- `feed_error` holds the wire `FeedError` object (`{"code": ...}`), and
-- `failing_since` the start of the error run that made the podcast failing.
-- Both are set exactly for `failing` rows: `ok`, `pending` and `gone` carry
-- no reason, and a `failing` row can't lack one.

alter table podcast
    add column failing_since timestamptz,
    add column feed_error    jsonb;

-- Rows that are failing today keep the reason their message named. The
-- messages were a closed set written by the poller, so each maps back to
-- its code; anything else dates from before the messages were fixed and
-- reads as a failed connection.
update podcast p
set failing_since = coalesce(
        (select date_trunc('milliseconds', f.first_error_at)
         from feed_poll f where f.podcast_id = p.id),
        date_trunc('milliseconds', p.updated_at)),
    feed_error = case
        when p.status_message ~ '^Feed returned HTTP [0-9]+$' then
            jsonb_build_object('code', 'http',
                'status', substring(p.status_message from '[0-9]+$')::int)
        when p.status_message = 'Not a podcast feed'  then '{"code": "not_a_feed"}'::jsonb
        when p.status_message = 'Invalid feed URL'    then '{"code": "invalid_url"}'::jsonb
        when p.status_message = 'Port not allowed'    then '{"code": "port_not_allowed"}'::jsonb
        when p.status_message = 'Address not allowed' then '{"code": "address_not_allowed"}'::jsonb
        when p.status_message = 'Host not found'      then '{"code": "host_not_found"}'::jsonb
        when p.status_message = 'Too many redirects'  then '{"code": "too_many_redirects"}'::jsonb
        when p.status_message = 'Invalid redirect'    then '{"code": "invalid_redirect"}'::jsonb
        when p.status_message = 'Feed is too large'   then '{"code": "too_large"}'::jsonb
        when p.status_message = 'Feed timed out'      then '{"code": "timeout"}'::jsonb
        else '{"code": "connection_failed"}'::jsonb
    end
where p.status = 'failing';

alter table podcast drop column status_message;

alter table podcast
    add constraint podcast_failing_carries_reason
    check ((status = 'failing') = (failing_since is not null and feed_error is not null));

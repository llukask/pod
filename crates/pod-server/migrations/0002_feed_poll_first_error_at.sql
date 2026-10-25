-- Poller state for the "7 days of continuous errors" rule (spec 3.2).
--
-- An `ok` podcast turns `failing` only after a week of errors in a row, and
-- `consecutive_errors` alone can't tell how long a run lasted, because the
-- backoff between polls grows with it. `first_error_at` records when the
-- current run of errors started; the poller sets it on the first error and
-- clears it on the next success.
--
-- This column is not in the spec 2.2 DDL. Like the rest of `feed_poll` it is
-- internal poller state and never sent to clients.

alter table feed_poll add column first_error_at timestamptz;

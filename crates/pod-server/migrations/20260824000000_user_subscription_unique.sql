-- The table ran without this constraint, so duplicate pairs may exist and
-- would block the index. The survivor is the earliest subscription, since
-- created_at is the user-visible fact of when they subscribed; id breaks
-- exact ties. Both columns are non-null, so the row comparison is a total
-- order over the duplicates.
delete from user_subscription a
using user_subscription b
where a.user_id = b.user_id
  and a.podcast_id = b.podcast_id
  and (a.created_at, a.id) > (b.created_at, b.id);

-- An old instance still serving during the deploy can commit a duplicate
-- through its read-then-insert add_subscription after this transaction's
-- snapshot, which fails the ALTER. The new binary then refuses to boot until
-- the migration is re-run against a database no longer taking those writes.
alter table user_subscription add constraint unique_user_subscription unique (user_id, podcast_id);

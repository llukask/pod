-- The table ran without this constraint, so duplicate pairs may exist and
-- would block the index. id is a non-null uuid rendered as text, so `>` is an
-- arbitrary but total order, which is all that picking one survivor requires.
delete from user_subscription a
using user_subscription b
where a.id > b.id
  and a.user_id = b.user_id
  and a.podcast_id = b.podcast_id;

alter table user_subscription add constraint unique_user_subscription unique (user_id, podcast_id);

-- Mutations the server will never accept (spec 5.6): `rejected` results and
-- outbox rows dropped after 5 failed attempts. Settings shows them as "N
-- changes couldn't be saved". The sync engine keeps the newest 50.

create table rejected_mutation (
    mutation_id text primary key,
    payload     text not null,      -- JSON mutation, as it sat in the outbox
    reason      text not null,      -- a rejection code, or `bad_request` after 5 attempts
    rejected_at integer not null    -- epoch ms
);

-- An index for the daily `applied_mutation` prune (spec 2.4).
--
-- Every pushed mutation leaves a row for 30 days, so this is the one table
-- that grows with use rather than with users. The prune deletes in batches
-- by `applied_at < cutoff`; without the index each batch would scan the
-- whole table.
--
-- This index is not in the spec 2.4 DDL. `session.expires_at` gets no index:
-- there is one session row per signed-in device, so the daily delete of
-- expired sessions scans a few hundred rows at most.

create index applied_mutation_applied_at_idx on applied_mutation (applied_at);

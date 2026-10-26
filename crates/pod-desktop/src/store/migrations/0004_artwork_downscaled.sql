-- The artwork cache stores images downscaled to the largest view
-- (`reconcile::artwork::MAX_SIDE`). Rows cached at full size are dropped:
-- the reconciler's startup sweep deletes their files, and the next pass
-- fetches the images again, downscaled.

delete from artwork;

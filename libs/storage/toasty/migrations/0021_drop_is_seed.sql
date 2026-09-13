-- `is_seed` is retired: the builtin demo app owns shipped content now
-- (0020), and the sync guard reads that ownership instead of the marker.
-- #[toasty::breakpoint]
ALTER TABLE tasks DROP COLUMN "is_seed";
-- #[toasty::breakpoint]
ALTER TABLE tags DROP COLUMN "is_seed";

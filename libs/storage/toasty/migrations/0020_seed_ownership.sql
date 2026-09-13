-- Move demo/seed content onto the builtin `demo` app (decision #33). This
-- is a bridge for databases seeded before the builtin app existed: it reads
-- `is_seed`, which 0021 drops, and writes the ownership columns 0019 added.
-- The demo app row is created here (idempotently) because migrations run
-- before `ensure_builtin_apps` on startup.
-- #[toasty::breakpoint]
INSERT INTO apps (kind, slug, label, description, enabled, created_at)
VALUES (
    'builtin',
    'demo',
    'Demo data',
    'Shipped example content; never syncs to integrations.',
    1,
    strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
)
ON CONFLICT (slug) DO UPDATE SET enabled = 1;
-- #[toasty::breakpoint]
-- Directory-backed tags stay user-owned (apps may not manage them), so
-- project tags are excluded even when they carry seed data.
UPDATE tags
SET managed_by = (SELECT id FROM apps WHERE slug = 'demo')
WHERE is_seed = 1 AND name NOT LIKE 'project:%';
-- #[toasty::breakpoint]
-- Captured, not managed: shipped tasks keep full edit rights and the app
-- never regenerates them; ownership only keeps them out of sync.
UPDATE tasks
SET managed_by = (SELECT id FROM apps WHERE slug = 'demo'),
    managed_mode = 'captured',
    managed_editable = 1
WHERE is_seed = 1;

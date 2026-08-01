-- Multi-tenancy: turn the implicit single tenant into a real registry.
--
-- `analytics_events.site_id` has always carried a tenant id, but it was checked
-- against an in-memory `ALLOWED_SITES` env list, and `blog_posts` / `products`
-- had no tenant at all — their `slug UNIQUE` was one global namespace, so two
-- tenants could never both own `/about`. This migration makes the tenant list a
-- table and pushes `site_id` down onto the content.
--
-- `sites.id` is `text`, not a surrogate key, because `analytics_events` already
-- stores the tenant as text and all six of its indexes lead with that column
-- (0001_init.sql:46-57). A surrogate would mean either a join on every stats
-- query in db.rs or backfilling an FK column onto the largest table in the
-- database — a full rewrite on a live server.
--
-- Deliberately NO foreign key from `analytics_events`. An FK takes a KEY SHARE
-- lock on the parent row for every insert, and every concurrent /collect for the
-- same site would contend on the same tuple. Worse, ingest is fire-and-forget
-- (ingest.rs) — the 202 is already sent, so a violation would be invisible to
-- the client. Ingest is gated synchronously in the handler against the cached
-- registry instead. `blog_posts` and `products` DO get one: tiny tables,
-- admin-only writes, and an orphaned row there would be silently unreachable
-- forever because every read filters by site.
--
-- Runs in one transaction (no `-- no-transaction` marker): every index built
-- here is on a table of tens of rows, so a sub-millisecond ACCESS EXCLUSIVE lock
-- is a better trade than losing atomicity to CREATE INDEX CONCURRENTLY. Per
-- AGENTS.md, CONCURRENTLY is for indexing a table that already has live traffic.

-- 1. The registry.
CREATE TABLE IF NOT EXISTS sites (
    id               text PRIMARY KEY,
    name             text NOT NULL DEFAULT '',
    domain           text,
    contact_to       text,
    email_from       text,
    email_from_name  text,
    admin_token_hash bytea,
    token_last4      text,
    token_rotated_at timestamptz,
    active           boolean NOT NULL DEFAULT true,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz,

    -- Mirrors types.rs MAX_SITE_ID = 64 and the `^[a-z0-9-]+$` slug rule already
    -- enforced by site_valid() in the application. Stricter than the ids that
    -- historically reached analytics_events, so the seed below filters rather
    -- than assuming — an id that fails this check must not abort startup.
    CONSTRAINT sites_id_check
        CHECK (id ~ '^[a-z0-9-]+$' AND length(id) <= 64),

    -- Both addresses are interpolated into outbound mail. `email_from` lands in
    -- a From: header (email.rs build_from_header), where a CR/LF would be header
    -- injection — this charset ban is a security control, not cosmetics. The
    -- application re-validates on load as defence in depth.
    CONSTRAINT sites_contact_to_check
        CHECK (contact_to IS NULL OR
               (length(contact_to) <= 254 AND
                contact_to ~ '^[^[:space:],<>]+@[^[:space:],<>]+\.[^[:space:],<>]+$')),
    CONSTRAINT sites_email_from_check
        CHECK (email_from IS NULL OR
               (length(email_from) <= 254 AND
                email_from ~ '^[^[:space:],<>]+@[^[:space:],<>]+\.[^[:space:],<>]+$'))
);

-- Token lookup is an index probe, and two sites can never share a credential.
-- Safe because the stored value is an unsalted SHA-256 of a 256-bit CSPRNG
-- token: deterministic, so it can be indexed. See docs/SECURITY.md for why a
-- salted KDF would be the wrong primitive here.
CREATE UNIQUE INDEX IF NOT EXISTS sites_admin_token_hash_key
    ON sites (admin_token_hash) WHERE admin_token_hash IS NOT NULL;

-- 2. Nullable tenant columns; NOT NULL comes after the backfill. There is no
--    tenant-agnostic literal to use, so a one-shot ADD COLUMN NOT NULL DEFAULT
--    is not available.
ALTER TABLE blog_posts ADD COLUMN IF NOT EXISTS site_id text;
ALTER TABLE products   ADD COLUMN IF NOT EXISTS site_id text;

-- 3. Seed the registry from the data, then adopt the pre-tenancy content.
DO $$
DECLARE
    primary_site text;
BEGIN
    -- Every site that has ever sent traffic is, by definition, a site this
    -- server serves. This is the honest reconstruction of the state that
    -- ALLOWED_SITES was meant to describe (and which is unset on the live
    -- deploy, which is why the weekly digest currently hard-fails).
    INSERT INTO sites (id, name)
    SELECT DISTINCT site_id, site_id
      FROM analytics_events
     WHERE site_id ~ '^[a-z0-9-]+$' AND length(site_id) <= 64
    ON CONFLICT (id) DO NOTHING;

    -- The tenant that owns content written before tenancy existed: the busiest
    -- site, ties broken alphabetically so the result is reproducible on a
    -- replica or a restored dump.
    SELECT site_id INTO primary_site
      FROM analytics_events
     WHERE site_id ~ '^[a-z0-9-]+$' AND length(site_id) <= 64
     GROUP BY site_id
     ORDER BY count(*) DESC, site_id ASC
     LIMIT 1;

    -- A database with content but no traffic (fresh install, restored partial
    -- dump) still needs somewhere to put it.
    IF primary_site IS NULL
       AND (EXISTS (SELECT 1 FROM blog_posts) OR EXISTS (SELECT 1 FROM products))
    THEN
        primary_site := 'default';
        INSERT INTO sites (id, name) VALUES ('default', 'default')
        ON CONFLICT (id) DO NOTHING;
    END IF;

    IF primary_site IS NOT NULL THEN
        UPDATE blog_posts SET site_id = primary_site WHERE site_id IS NULL;
        UPDATE products   SET site_id = primary_site WHERE site_id IS NULL;
    END IF;
END $$;

-- 4. Content with no tenant is unreachable, so the column is mandatory. Note
--    there is deliberately no column DEFAULT: the application always binds
--    site_id explicitly, and a DEFAULT would turn "forgot to bind the tenant"
--    from a loud 23502 into a silent write into the wrong tenant — in exactly
--    the code path this migration exists to harden.
ALTER TABLE blog_posts ALTER COLUMN site_id SET NOT NULL;
ALTER TABLE products   ALTER COLUMN site_id SET NOT NULL;

-- 5. Referential integrity for the low-volume tables only (see header).
--    RESTRICT, not CASCADE: deleting a tenant must fail loudly rather than
--    silently vaporize their blog. Offboarding is an explicit purge.
--    ON UPDATE CASCADE makes a site rename one statement per table.
ALTER TABLE blog_posts ADD CONSTRAINT blog_posts_site_id_fkey
    FOREIGN KEY (site_id) REFERENCES sites (id)
    ON UPDATE CASCADE ON DELETE RESTRICT;
ALTER TABLE products ADD CONSTRAINT products_site_id_fkey
    FOREIGN KEY (site_id) REFERENCES sites (id)
    ON UPDATE CASCADE ON DELETE RESTRICT;

-- 6. Retire the global slug namespace. Postgres auto-named these from the
--    column-level `slug text UNIQUE` in 0001/0003. Dropped by discovered name
--    rather than a hardcoded one: a silent no-op here would leave the global
--    namespace in place, and the bug stays invisible until two tenants collide.
DO $$
DECLARE c record;
BEGIN
    FOR c IN
        SELECT rel.relname AS tbl, con.conname AS name
          FROM pg_constraint con
          JOIN pg_class rel ON rel.oid = con.conrelid
          JOIN pg_attribute att ON att.attrelid = rel.oid AND att.attnum = con.conkey[1]
         WHERE con.contype = 'u'
           AND rel.relname IN ('blog_posts', 'products')
           AND array_length(con.conkey, 1) = 1
           AND att.attname = 'slug'
    LOOP
        EXECUTE format('ALTER TABLE %I DROP CONSTRAINT %I', c.tbl, c.name);
    END LOOP;
END $$;

-- 7. Slugs are unique per tenant. This also serves `WHERE site_id = $n AND
--    slug = $m` on the single-item GET and the view counter — site_id leads, so
--    the index is usable for both.
ALTER TABLE blog_posts ADD CONSTRAINT blog_posts_site_slug_key UNIQUE (site_id, slug);
ALTER TABLE products   ADD CONSTRAINT products_site_slug_key   UNIQUE (site_id, slug);

-- 8. Listing indexes gain the tenant as leading column, mirroring the
--    analytics_events index set. The old ones are dropped rather than kept:
--    with `WHERE site_id = $n` on every listing query they can never be chosen,
--    and a dead index is still a write cost.
DROP INDEX IF EXISTS blog_posts_published_idx;
CREATE INDEX IF NOT EXISTS blog_posts_site_published_idx
    ON blog_posts (site_id, draft, pub_date DESC);

DROP INDEX IF EXISTS products_listing_idx;
CREATE INDEX IF NOT EXISTS products_site_listing_idx
    ON products (site_id, draft, position, created_at DESC);

-- Full schema, squashed. Everything dullahan needs: the tenant registry, the
-- analytics event table and its indexes, the daily salt table behind opt-in
-- anonymized sessions, the blog and product content tables, and per-site config.
--
-- Two identifiers at different grains live on analytics_events:
--   view_id      — one pageload / SPA navigation. Always set, never durable.
--   visitor_hash — H(daily salt, site, ip, ua): one value per visitor per UTC
--                  day, set only when SESSIONS_ENABLED. Raw IPs are never stored.

-- ---------------------------------------------------------------------------
-- The tenant registry. Everything else references it, so it comes first.
-- ---------------------------------------------------------------------------
--
-- `sites.id` is `text`, not a surrogate key, because `analytics_events` stores
-- the tenant as text and all seven of its indexes lead with that column. A
-- surrogate would mean either a join on every stats query in db.rs or an FK
-- column on the largest table in the database.
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

    -- Mirrors types.rs MAX_SITE_ID = 64 and the `^[a-z0-9-]+$` slug rule
    -- enforced by site_valid() in the application.
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

-- ---------------------------------------------------------------------------
-- Analytics events.
-- ---------------------------------------------------------------------------
--
-- Deliberately NO foreign key to `sites`. An FK takes a KEY SHARE lock on the
-- parent row for every insert, and every concurrent /collect for the same site
-- would contend on the same tuple. Worse, ingest is fire-and-forget (ingest.rs)
-- — the 202 is already sent, so a violation would be invisible to the client.
-- Ingest is gated synchronously in the handler against the cached registry
-- instead. The content tables below DO get one: tiny tables, admin-only writes,
-- and an orphaned row there would be silently unreachable forever because every
-- read filters by site.
CREATE TABLE IF NOT EXISTS analytics_events (
    id           bigserial PRIMARY KEY,
    site_id      text NOT NULL,
    type         text NOT NULL,
    path         text NOT NULL,
    ts           bigint NOT NULL,
    referrer     text,
    device       text,
    event_name   text,
    event_props  jsonb,
    country      text,
    received_at  timestamptz NOT NULL DEFAULT now(),
    dur_ms       int,
    utm_source   text,
    utm_medium   text,
    utm_campaign text,
    view_id      text,
    visitor_hash text,
    -- The three kinds RawPayload (types.rs) can produce. Web vitals were cut
    -- along with the vitals/heatmap/engagement/funnel endpoints, so there is no
    -- 'performance' kind and no `metrics`/`browser`/`os`/`viewport` column.
    CONSTRAINT analytics_events_type_check
        CHECK (type IN ('pageview', 'event', 'pageleave')),
    CONSTRAINT analytics_events_device_check
        CHECK (device IN ('mobile', 'tablet', 'desktop')),
    CONSTRAINT analytics_events_dur_ms_check
        CHECK (dur_ms IS NULL OR (dur_ms >= 0 AND dur_ms <= 1800000))
);

-- This index set was settled with EXPLAIN ANALYZE, not intuition. `received_at`
-- needs its own index because /stats/realtime filters on server receive time
-- while every other query filters the client clock. Engagement deliberately
-- gets no (site_id, view_id) index: it selects by (site_id, ts) — covered here —
-- then groups by view_id, which such an index does not speed up while costing a
-- random-UUID write on every insert.
CREATE INDEX IF NOT EXISTS analytics_events_site_ts_idx
    ON analytics_events (site_id, ts DESC);
CREATE INDEX IF NOT EXISTS analytics_events_site_path_ts_idx
    ON analytics_events (site_id, path, ts DESC);
CREATE INDEX IF NOT EXISTS analytics_events_site_type_ts_idx
    ON analytics_events (site_id, type, ts DESC);
CREATE INDEX IF NOT EXISTS analytics_events_site_event_name_ts_idx
    ON analytics_events (site_id, type, event_name, ts DESC);
CREATE INDEX IF NOT EXISTS analytics_events_site_visitor_ts_idx
    ON analytics_events (site_id, visitor_hash, ts DESC);
CREATE INDEX IF NOT EXISTS analytics_events_site_received_idx
    ON analytics_events (site_id, received_at DESC);

-- The one index not led by site_id: the RETENTION_DAYS sweep deletes by client
-- timestamp across all tenants (db::prune_events), so every (site_id, …) index
-- above is useless to it. Cheap to maintain — `ts` is approximately now() on
-- every insert, so entries land on the right edge of the tree rather than
-- scattering across it.
CREATE INDEX IF NOT EXISTS analytics_events_ts_idx
    ON analytics_events (ts);

-- Random salt per UTC day, pruned after retention: once a day's salt is gone,
-- its visitor hashes can never be recomputed or re-linked.
CREATE TABLE IF NOT EXISTS daily_salts (
    day        date PRIMARY KEY,
    salt       bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Content: blog posts and products.
-- ---------------------------------------------------------------------------
--
-- Note there is deliberately no DEFAULT on `site_id`: the application always
-- binds it explicitly, and a DEFAULT would turn "forgot to bind the tenant"
-- from a loud 23502 into a silent write into the wrong tenant.
--
-- The FK is RESTRICT, not CASCADE: deleting a tenant must fail loudly rather
-- than silently vaporize their content. Offboarding is an explicit purge.
-- ON UPDATE CASCADE makes a site rename one statement per table.
--
-- `slug` is unique *per tenant*, never globally — two tenants must both be able
-- to own `/about`. The unique constraint also serves `WHERE site_id = $n AND
-- slug = $m` on the single-item GET and the view counter, since site_id leads.
CREATE TABLE IF NOT EXISTS blog_posts (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    site_id       text NOT NULL REFERENCES sites (id)
                      ON UPDATE CASCADE ON DELETE RESTRICT,
    slug          text NOT NULL,
    title         text NOT NULL,
    description   text NOT NULL DEFAULT '',
    author        text NOT NULL DEFAULT '',
    image         text,
    body_markdown text NOT NULL,
    draft         boolean NOT NULL DEFAULT false,
    views         bigint NOT NULL DEFAULT 0,
    pub_date      timestamptz NOT NULL DEFAULT now(),
    updated_date  timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT blog_posts_site_slug_key UNIQUE (site_id, slug)
);

-- Drives the published list: WHERE site_id = $1 AND draft = false
-- ORDER BY pub_date DESC.
CREATE INDEX IF NOT EXISTS blog_posts_site_published_idx
    ON blog_posts (site_id, draft, pub_date DESC);

-- Product catalog for the headless webshop (`/products`): a flat list of items
-- with an image URL, title, description, and price. No cart, no orders, no
-- stock counts — just a listing the frontend renders.
--
-- Price is stored as integer minor units (`price_cents`) to avoid float money
-- bugs; the shop's single currency lives in config (`SHOP_CURRENCY`), not per
-- row. `available` flags sold-out items (kept visible, greyed by the frontend);
-- `draft` hides an item from the public list entirely, mirroring blog_posts.
CREATE TABLE IF NOT EXISTS products (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    site_id      text NOT NULL REFERENCES sites (id)
                     ON UPDATE CASCADE ON DELETE RESTRICT,
    slug         text NOT NULL,
    title        text NOT NULL,
    description  text NOT NULL DEFAULT '',
    image        text,
    price_cents  bigint NOT NULL DEFAULT 0,
    available    boolean NOT NULL DEFAULT true,
    position     integer NOT NULL DEFAULT 0,
    draft        boolean NOT NULL DEFAULT false,
    views        bigint NOT NULL DEFAULT 0,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_date timestamptz,
    CONSTRAINT products_price_cents_check CHECK (price_cents >= 0),
    CONSTRAINT products_site_slug_key UNIQUE (site_id, slug)
);

-- Drives the public listing: WHERE site_id = $1 AND draft = false
-- ORDER BY position, created_at DESC.
CREATE INDEX IF NOT EXISTS products_site_listing_idx
    ON products (site_id, draft, position, created_at DESC);

-- ---------------------------------------------------------------------------
-- Per-site configuration for the config-driven storefront + its dashboard.
-- ---------------------------------------------------------------------------
--
-- Each row is one site's *entire* config document, stored as an opaque JSON
-- blob: dullahan does not know or validate the schema — the TypeScript
-- `SiteConfig` type is the contract, shared by the storefront that reads the
-- config and the dashboard that writes it. Public reads (a storefront fetches
-- its own config), admin-gated writes (like products/blog).
CREATE TABLE IF NOT EXISTS site_config (
    site       text PRIMARY KEY REFERENCES sites (id)
                   ON UPDATE CASCADE ON DELETE RESTRICT,
    config     jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Full schema, squashed from the original seven migrations: the analytics event
-- table and its indexes, the daily salt table behind opt-in anonymized
-- sessions, and the blog table.
--
-- Two identifiers at different grains live on analytics_events:
--   view_id      — one pageload / SPA navigation. Always set, never durable.
--   visitor_hash — H(daily salt, site, ip, ua): one value per visitor per UTC
--                  day, set only when SESSIONS_ENABLED. Raw IPs are never stored.

CREATE TABLE IF NOT EXISTS analytics_events (
    id           bigserial PRIMARY KEY,
    site_id      text NOT NULL,
    type         text NOT NULL,
    path         text NOT NULL,
    ts           bigint NOT NULL,
    referrer     text,
    device       text,
    viewport     int,
    event_name   text,
    event_props  jsonb,
    metrics      jsonb,
    country      text,
    received_at  timestamptz NOT NULL DEFAULT now(),
    dur_ms       int,
    utm_source   text,
    utm_medium   text,
    utm_campaign text,
    view_id      text,
    visitor_hash text,
    browser      text,
    os           text,
    CONSTRAINT analytics_events_type_check
        CHECK (type IN ('pageview', 'event', 'performance', 'pageleave')),
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

-- Random salt per UTC day, pruned after retention: once a day's salt is gone,
-- its visitor hashes can never be recomputed or re-linked.
CREATE TABLE IF NOT EXISTS daily_salts (
    day        date PRIMARY KEY,
    salt       bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS blog_posts (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    slug          text UNIQUE NOT NULL,
    title         text NOT NULL,
    description   text NOT NULL DEFAULT '',
    author        text NOT NULL DEFAULT '',
    image         text,
    body_markdown text NOT NULL,
    draft         boolean NOT NULL DEFAULT false,
    views         bigint NOT NULL DEFAULT 0,
    pub_date      timestamptz NOT NULL DEFAULT now(),
    updated_date  timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now()
);

-- Drives the published-list query: WHERE draft = false ORDER BY pub_date DESC.
CREATE INDEX IF NOT EXISTS blog_posts_published_idx ON blog_posts (draft, pub_date DESC);

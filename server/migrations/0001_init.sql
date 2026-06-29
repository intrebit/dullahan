CREATE TABLE IF NOT EXISTS sites (
    site_id     text PRIMARY KEY,
    name        text NOT NULL,
    domain      text,
    owner_email text,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS analytics_events (
    id           bigserial PRIMARY KEY,
    site_id      text NOT NULL,
    type         text NOT NULL CHECK (type IN ('pageview', 'event', 'performance')),
    path         text NOT NULL,
    ts           bigint NOT NULL,
    referrer     text,
    device       text CHECK (device IN ('mobile', 'tablet', 'desktop')),
    viewport     int,
    event_name   text,
    event_props  jsonb,
    metrics      jsonb,
    country      text,
    received_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS analytics_events_site_ts_idx       ON analytics_events (site_id, ts DESC);
CREATE INDEX IF NOT EXISTS analytics_events_site_path_ts_idx  ON analytics_events (site_id, path, ts DESC);
CREATE INDEX IF NOT EXISTS analytics_events_site_type_ts_idx  ON analytics_events (site_id, type, ts DESC);

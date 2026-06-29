-- Rung 2 (opt-in, SESSIONS_ENABLED): anonymized sessions via a salted daily
-- hash. The salt is random per UTC day and deleted after 48h, so a visitor
-- hash becomes permanently unlinkable once its salt rotates out. Raw IPs are
-- never stored — only the hash derived from (salt, site_id, ip, user-agent).
CREATE TABLE IF NOT EXISTS daily_salts (
    day        date PRIMARY KEY,
    salt       bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE analytics_events ADD COLUMN visitor_hash text;
ALTER TABLE analytics_events ADD COLUMN browser      text;
ALTER TABLE analytics_events ADD COLUMN os           text;

-- Backs unique-visitor / session aggregates.
CREATE INDEX IF NOT EXISTS analytics_events_site_visitor_ts_idx
    ON analytics_events (site_id, visitor_hash, ts DESC);

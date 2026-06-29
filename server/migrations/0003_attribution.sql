ALTER TABLE analytics_events ADD COLUMN utm_source   text;
ALTER TABLE analytics_events ADD COLUMN utm_medium   text;
ALTER TABLE analytics_events ADD COLUMN utm_campaign text;

-- Backs /stats/events: breakdowns filter by (site_id, type='event', event_name)
-- and order by ts.
CREATE INDEX IF NOT EXISTS analytics_events_site_event_name_ts_idx
    ON analytics_events (site_id, type, event_name, ts DESC);

ALTER TABLE analytics_events DROP CONSTRAINT analytics_events_type_check;

ALTER TABLE analytics_events
    ADD CONSTRAINT analytics_events_type_check
    CHECK (type IN ('pageview', 'event', 'performance', 'pageleave')) NOT VALID;

ALTER TABLE analytics_events VALIDATE CONSTRAINT analytics_events_type_check;

ALTER TABLE analytics_events ADD COLUMN dur_ms int;

ALTER TABLE analytics_events
    ADD CONSTRAINT analytics_events_dur_ms_check
    CHECK (dur_ms IS NULL OR (dur_ms >= 0 AND dur_ms <= 1800000));

-- Trim the analytics surface to the core metric set. Web-vitals (`metrics`),
-- UA-derived `browser`/`os`, and raw `viewport` are no longer collected or
-- queried after the vitals/heatmap/engagement/sessions/funnel endpoints were cut.
--
-- DROP COLUMN is metadata-only in Postgres (no table rewrite), so this is fast
-- and safe on the live table. Existing `type = 'performance'` rows are left in
-- place — harmless and unqueried; the `type` CHECK still permits them, so no
-- constraint change is needed.
ALTER TABLE analytics_events
    DROP COLUMN IF EXISTS metrics,
    DROP COLUMN IF EXISTS browser,
    DROP COLUMN IF EXISTS os,
    DROP COLUMN IF EXISTS viewport;

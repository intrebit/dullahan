-- no-transaction
-- Real-time active (/stats/realtime) filters analytics_events by the server
-- receive time (received_at). Every existing index is on the client-supplied
-- `ts`, so without this the query scans all of a site's rows. CONCURRENTLY so
-- building it on a large live table does not block /collect inserts; that
-- cannot run inside a transaction, hence the `-- no-transaction` directive
-- above (sqlx 0.8 honours it).
--
-- Per-page-visit engagement (/stats/engagement) deliberately gets no new index:
-- it selects rows by (site_id, ts) — already covered — then aggregates by
-- view_id, a plan a (site_id, view_id) index would not improve.
--
-- Operator note: if a CONCURRENTLY build is interrupted it leaves an INVALID
-- index that `IF NOT EXISTS` then skips. Drop it and re-run:
--   DROP INDEX analytics_events_site_received_idx; -- then restart the server
CREATE INDEX CONCURRENTLY IF NOT EXISTS analytics_events_site_received_idx
    ON analytics_events (site_id, received_at DESC);

-- Ephemeral per-view id (rung 1): a random id regenerated on every page load /
-- SPA navigation, attached to all events within that view. In-memory on the
-- client, never persisted there, never a durable visitor identity. Lets the
-- read side join events that happened within a single pageload.
ALTER TABLE analytics_events ADD COLUMN view_id text;

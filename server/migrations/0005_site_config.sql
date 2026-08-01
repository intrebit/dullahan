-- Per-site configuration for the config-driven storefront + its dashboard.
-- Each row is one site's *entire* config document, stored as an opaque JSON
-- blob: dullahan does not know or validate the schema — the TypeScript
-- `SiteConfig` type is the contract, shared by the storefront that reads the
-- config and the dashboard that writes it. Public reads (a storefront fetches
-- its own config), admin-gated writes (like products/blog).
--
-- `site` references the tenant registry introduced in 0004 — the same handle
-- used for analytics, content and contact routing. RESTRICT rather than CASCADE
-- for the same reason as blog_posts/products: removing a tenant must fail loudly
-- while they still have state, not quietly discard it.
CREATE TABLE IF NOT EXISTS site_config (
    site       text PRIMARY KEY REFERENCES sites (id)
                   ON UPDATE CASCADE ON DELETE RESTRICT,
    config     jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

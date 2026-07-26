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
    slug         text UNIQUE NOT NULL,
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
    CONSTRAINT products_price_cents_check CHECK (price_cents >= 0)
);

-- Drives the public listing: WHERE draft = false ORDER BY position, created_at DESC.
CREATE INDEX IF NOT EXISTS products_listing_idx ON products (draft, position, created_at DESC);

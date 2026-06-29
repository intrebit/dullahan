CREATE TABLE IF NOT EXISTS blog_posts (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    slug          text UNIQUE NOT NULL,
    title         text NOT NULL,
    description   text NOT NULL DEFAULT '',
    author        text NOT NULL DEFAULT 'Andrej Focic',
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

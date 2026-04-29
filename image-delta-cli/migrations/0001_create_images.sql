CREATE TABLE IF NOT EXISTS images (
    image_id TEXT PRIMARY KEY,
    base_image_id TEXT,
    format TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    status_detail TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
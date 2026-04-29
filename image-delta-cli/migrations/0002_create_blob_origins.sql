CREATE TABLE IF NOT EXISTS blob_origins (
    blob_id UUID NOT NULL,
    image_id TEXT NOT NULL REFERENCES images(image_id),
    path TEXT NOT NULL,
    size BIGINT NOT NULL,
    PRIMARY KEY (blob_id, image_id)
);
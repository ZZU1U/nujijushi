ALTER TABLE posts ADD COLUMN caption_url TEXT;

CREATE INDEX idx_jobs_completed_import_post ON jobs (post_id, id DESC)
    WHERE kind = 'import' AND state = 'done' AND payload IS NOT NULL;

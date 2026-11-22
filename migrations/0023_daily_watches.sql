ALTER TABLE watched_sources ADD COLUMN last_run_at TEXT;

-- Older jobs only recorded their final update time. Use it for the first daily interval.
WITH last_checks AS (
    SELECT payload, MAX(id) AS id
    FROM jobs
    WHERE kind = 'watch_check' AND state IN ('done', 'failed')
    GROUP BY payload
)
UPDATE watched_sources AS watch
SET last_run_at = job.updated_at
FROM last_checks JOIN jobs AS job ON job.id = last_checks.id
WHERE CAST(watch.id AS TEXT) = last_checks.payload;

-- Durations (RSS has none): devices look them up from a channel's tabs, one
-- device per channel at a time (feeds.rs, "duration jobs").

-- A device looked for this video's duration (found or not): never asked again.
ALTER TABLE feed_items ADD COLUMN duration_checked boolean NOT NULL DEFAULT false;

-- The device (token) that has the channel's duration job, until when.
ALTER TABLE feed_channels ADD COLUMN enrich_until timestamptz;
ALTER TABLE feed_channels ADD COLUMN enrich_token uuid;

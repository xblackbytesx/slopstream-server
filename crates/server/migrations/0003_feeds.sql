-- The video lists the server fetches (feeds.rs). Lists only: streams are
-- always resolved on the device. Times in epoch ms like the library, except
-- the schedule columns, which only the server reads.

-- `owner`: the nil uuid for YouTube (one fetch serves everyone who follows the
-- channel); the person for Floatplane and Sauce+, whose lists depend on that
-- person's plan and are fetched with their own session.
CREATE TABLE feed_channels (
    source         text NOT NULL,
    remote_id      text NOT NULL,
    owner          uuid NOT NULL,
    name           text,
    next_fetch_at  timestamptz NOT NULL DEFAULT now(),
    last_fetch_at  timestamptz,
    last_ok_at     timestamptz,
    -- Newest upload seen: quiet channels are fetched less often.
    last_upload_at bigint,
    failures       integer NOT NULL DEFAULT 0,
    last_error     text,
    -- Nobody follows it any more; dropped with its items after 7 days.
    unfollowed_at  timestamptz,
    PRIMARY KEY (source, remote_id, owner)
);
CREATE INDEX feed_channels_due ON feed_channels (source, next_fetch_at) WHERE unfollowed_at IS NULL;

-- Every change takes the next value; devices ask per channel for revs above
-- what they hold.
CREATE SEQUENCE feed_rev;

CREATE TABLE feed_items (
    owner         uuid NOT NULL,
    source        text NOT NULL,
    remote_id     text NOT NULL,
    channel_id    text NOT NULL,
    channel_name  text NOT NULL,
    title         text NOT NULL,
    published_at  bigint NOT NULL,
    kind          text NOT NULL,
    thumbnail_url text,
    duration_sec  bigint,
    fetched_at    bigint NOT NULL,
    -- An ended livestream. Tombstones sync, then go after 7 days.
    deleted_at    bigint,
    rev           bigint NOT NULL,
    PRIMARY KEY (owner, source, remote_id)
);
CREATE INDEX feed_items_channel_rev ON feed_items (owner, source, channel_id, rev);
CREATE INDEX feed_items_channel_kind ON feed_items (owner, source, channel_id, kind, published_at DESC);

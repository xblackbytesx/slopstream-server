-- Each person's library, as the app's Room rows plus the server's `rev`.
-- Times are epoch milliseconds on the server's clock (devices' stamps are
-- shifted on the way in, see slopstream_core::merge::Clock). Rows are never
-- hard-deleted: `deleted_at` is the tombstone, and it syncs.

-- Every accepted write takes the next value. Writes for one person are
-- serialized (an advisory lock per person), so a device's cursor never
-- skips a row committed late.
CREATE SEQUENCE sync_rev;

CREATE TABLE subscriptions (
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    source     text NOT NULL,
    remote_id  text NOT NULL,
    name       text NOT NULL,
    avatar_url text,
    favorite   boolean NOT NULL DEFAULT false,
    added_at   bigint NOT NULL,
    updated_at bigint NOT NULL,
    deleted_at bigint,
    device_id  text NOT NULL,
    rev        bigint NOT NULL,
    PRIMARY KEY (user_id, source, remote_id)
);
CREATE INDEX subscriptions_rev ON subscriptions (user_id, rev);

CREATE TABLE watch_later (
    user_id       uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    source        text NOT NULL,
    remote_id     text NOT NULL,
    title         text NOT NULL,
    channel_id    text NOT NULL,
    channel_name  text NOT NULL,
    thumbnail_url text,
    duration_sec  bigint,
    published_at  bigint NOT NULL,
    kind          text NOT NULL,
    added_at      bigint NOT NULL,
    updated_at    bigint NOT NULL,
    deleted_at    bigint,
    device_id     text NOT NULL,
    rev           bigint NOT NULL,
    PRIMARY KEY (user_id, source, remote_id)
);
CREATE INDEX watch_later_rev ON watch_later (user_id, rev);

CREATE TABLE watch_state (
    user_id       uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    source        text NOT NULL,
    remote_id     text NOT NULL,
    position_ms   bigint NOT NULL,
    duration_ms   bigint NOT NULL,
    watched       boolean NOT NULL,
    title         text,
    channel_id    text,
    channel_name  text,
    thumbnail_url text,
    kind          text,
    updated_at    bigint NOT NULL,
    deleted_at    bigint,
    device_id     text NOT NULL,
    rev           bigint NOT NULL,
    PRIMARY KEY (user_id, source, remote_id)
);
CREATE INDEX watch_state_rev ON watch_state (user_id, rev);
CREATE INDEX watch_state_history ON watch_state (user_id, source, updated_at DESC);

-- A person's own Floatplane / Sauce+ session, shared with their devices only.
-- `ciphertext`: XChaCha20-Poly1305 under SLOPSTREAM_SECRET_KEY, bound to
-- (user_id, source). `fingerprint`: hex SHA-256 of the value.
CREATE TABLE service_sessions (
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    source      text NOT NULL,
    ciphertext  bytea NOT NULL,
    fingerprint text NOT NULL,
    username    text,
    status      text NOT NULL CHECK (status IN ('ok', 'expired', 'unchecked')),
    checked_at  timestamptz,
    updated_at  bigint NOT NULL,
    PRIMARY KEY (user_id, source)
);

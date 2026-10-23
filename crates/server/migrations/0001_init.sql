-- People, web logins, app tokens, invites and device pairing.

CREATE TABLE users (
    id            uuid PRIMARY KEY,
    email         text NOT NULL UNIQUE,
    display_name  text NOT NULL DEFAULT '',
    role          text NOT NULL DEFAULT 'user' CHECK (role IN ('admin', 'user')),
    -- NULL: web login disabled until a password is set.
    password_hash text,
    tz            text NOT NULL DEFAULT 'UTC',
    created_at    timestamptz NOT NULL DEFAULT now(),
    disabled_at   timestamptz
);

-- Browser sessions: only the SHA-256 of the cookie value is stored.
CREATE TABLE web_sessions (
    token_sha256  bytea PRIMARY KEY,
    user_id       uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at    timestamptz NOT NULL DEFAULT now(),
    expires_at    timestamptz NOT NULL,
    -- Wrong pair codes typed in this session; at 5 it stops accepting them.
    pair_failures integer NOT NULL DEFAULT 0
);
CREATE INDEX web_sessions_user ON web_sessions (user_id);

-- Bearer tokens for /api/v1, one per device. Hash only.
CREATE TABLE api_tokens (
    id           uuid PRIMARY KEY,
    user_id      uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    label        text NOT NULL,
    token_sha256 bytea NOT NULL UNIQUE,
    -- Made by pairing (true) or created on the website and pasted (false).
    paired       boolean NOT NULL DEFAULT false,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    revoked_at   timestamptz
);
CREATE INDEX api_tokens_user ON api_tokens (user_id);

-- One-time invite links made by an admin. Hash only; single use, expiring.
CREATE TABLE invites (
    id           uuid PRIMARY KEY,
    token_sha256 bytea NOT NULL UNIQUE,
    role         text NOT NULL DEFAULT 'user' CHECK (role IN ('admin', 'user')),
    note         text NOT NULL DEFAULT '',
    created_by   uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    used_at      timestamptz,
    used_by      uuid REFERENCES users (id) ON DELETE SET NULL
);

-- A device asking to be paired: it shows the user code, a signed-in person
-- types it on the website. Both codes are stored hashed. The token is made
-- on the first poll after approval, so no plaintext is ever stored.
CREATE TABLE pairings (
    id                 uuid PRIMARY KEY,
    device_code_sha256 bytea NOT NULL UNIQUE,
    user_code_sha256   bytea NOT NULL UNIQUE,
    device_name        text NOT NULL,
    created_at         timestamptz NOT NULL DEFAULT now(),
    expires_at         timestamptz NOT NULL,
    approved_by        uuid REFERENCES users (id) ON DELETE CASCADE,
    approved_at        timestamptz,
    denied_at          timestamptz,
    delivered_at       timestamptz
);

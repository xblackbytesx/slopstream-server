# Slopstream server: agent guide and decision register

The optional server for Slopstream (`/workspace/slopstream`, the Android app
for phones and TVs). It gives each person in a household one library across
their devices: subscriptions (with favorites), Watch later, watch progress and
history, plus their own Floatplane and Sauce+ sessions, signed in once and
used by every device of theirs. Since 0.2.0 it also fetches the video lists
all day, so devices find them ready. The website manages that state; nothing
plays on it. The app works fully without a server.

This file is the contract. Record decisions here when they are made, with the
date and the reason, and keep it true to the code. Never use em dashes.

## Build and verify

Everything runs through `make` (docker compose); no host toolchain is needed.

| Target | What |
|---|---|
| `make dev` | bacon hot reload + Postgres 17 on `${PORT:-8080}` |
| `make test` | `cargo test --workspace --release` incl. Postgres integration tests |
| `make lint` | `cargo fmt --check` + `clippy -D warnings` (the CI gate) |
| `make image` / `up` / `down` | build and smoke-test the production image locally |
| `make admin EMAIL=..` / `make password EMAIL=..` | first account, then its password (stdin) |
| `make secret` | a fresh `SLOPSTREAM_SECRET_KEY` |

CI: `.gitea/workflows/ci.yml` runs fmt, clippy and the tests (with a Postgres
service) on every push. `release.yml` on a `vX.Y.Z` tag runs the same gate,
checks the tag equals `[workspace.package] version`, then pushes the amd64 +
arm64 image `code.dropdoos.nl/xblackbytesx/slopstream-server`.

Agent sandbox fallback (no Docker): rustup toolchain `1.92.0`, a scratch
`CARGO_TARGET_DIR`, and a throwaway Postgres 17 from the zonky
embedded-postgres binaries; point `SLOPSTREAM_TEST_DATABASE_URL` at its
`postgres` database. Integration tests create and drop their own database and
print `SKIP` when the variable is unset. They use in-process stand-ins for
Floatplane's platform and YouTube (`tests/common`); real services are
exercised by the owner. Never relax a test to make it pass.

Golden files (`crates/core/tests/golden/*.json`) are the wire contract; the
app's contract test parses copies of them. Regenerate only on purpose:
`UPDATE_GOLDEN=1 cargo test -p slopstream-core`, then copy them into the app
(`app/src/test/resources/sync/`).

## Crates

| Crate | Role | Rule |
|---|---|---|
| `crates/core` (`slopstream-core`) | wire types, the merge rule, clock correction, services, pasted-session parsing, YouTube channel references, feed parsing (`feeds`) | Pure: no IO, no clock, no randomness |
| `crates/server` (`slopstream-server`) | axum + sqlx + askama/HTMX website, `/api/v1`, the session checker, CLI | Runtime sqlx queries only (no `query!`), so builds never need a database |

Pins (same as kagami-web and quantifyme-v2): rust 1.92, axum 0.8, sqlx 0.8
(0.9 needs rustc 1.94), askama 0.16, reqwest 0.13 on rustls/ring (aws-lc needs
cmake in every build image). HTMX 2.0.4 vendored (`static/htmx.min.js`, sha256
`e209dda5c8235479f3166defc7750e1dbcd5a5c1808b7792fc2e6733768fb447`).

## Layout

- `src/library.rs`: the synced tables (`Stored` maps each wire row to its
  columns), `sync`, and the website's `edit` / `follow`.
- `src/sessions.rs`: service sessions; `src/sites.rs`: session checks and
  sign-out at Floatplane's platform; `src/youtube.rs`: resolving a channel to
  follow; `src/feeds.rs`: the feed fetcher and `/api/v1/feed`.
- `src/changes.rs`: long-poll counters (`/api/v1/changes`).
- `src/pairing.rs`, `src/api.rs` (`/api/v1`), `src/web/*` (pages).
- `migrations/`: numbered SQL, embedded and run at every start.

## Decisions

Settled 2026-10-23 (plan approved by the owner; do not reopen):

1. **Stack and repo**: Rust + HTMX like kagami-web, in its own repo next to
   the app, with CI from day one. GPL-3.0-or-later like the app.
2. **One library per person.** Admins invite people (one-time links) and
   manage accounts, never other people's libraries or sessions.
3. **Service sessions are per person**, shared between that person's devices
   only, never with other people in the house.
4. **Devices pair with a code** (the TV shows it, the person types it on the
   website) and get their own token. A token made on the website and pasted
   into the app also works. Tokens are revocable per device.
5. **The server leads** and is optional: the app keeps working without it,
   and disconnecting a device keeps its library.
6. **Services stay hard-split** here too: every row names its service, and
   each page shows one service at a time (tabs).

### Sync (`slopstream_core::{wire, merge}`, `library::sync`)

- One round trip, `POST /api/v1/sync`: the device sends its unsynced rows,
  each with the `rev` it last saw for that row (`baseRev`, 0 if never). The
  server merges them, then returns every row of this person with
  `rev > cursor` (at most `PAGE`, `more` says sync again) plus the full list
  of sessions. The device applies the answer as is.
- **Merge rule** (`merge::decide`): no server row, insert. `baseRev` equals
  the server's rev, apply. Otherwise someone else wrote it since: the device
  wins only if its corrected `updatedAt` is more than `DOUBT_MS` (2 s) newer;
  else the server's row stays and goes back to the device in the same answer.
- **Clocks** (`merge::Clock`): `updatedAt` is shifted by
  `serverNow - clientNow` of that request and capped at now. Other dates are
  only capped: they travel back and forth unchanged, and shifting them every
  trip made them drift (caught by `unchanged_rows_get_no_new_rev`).
- A write identical to the server's row apart from stamp and device gets no
  new rev (no churn on other devices); it is echoed back instead.
- Website edits go through the same lock and rev, with device id `web` and
  `updatedAt` = now.

### Service sessions (`sessions.rs`)

- XChaCha20-Poly1305 under `SLOPSTREAM_SECRET_KEY`, bound to (person,
  service); `fingerprint` = hex SHA-256 of the value (`core::fingerprint`,
  the app computes the same).
- In: a device after its WebView sign-in (`PUT /api/v1/sessions/{source}`)
  or a paste on the website. The server checks it with `user/self` first: a
  refusal is an error; a challenge or outage stores it as `unchecked`.
- Out: only inside `/sync` to that person's tokens, and only when the
  device's fingerprint differs.
- Expiry: a device reports it with the fingerprint it used. A different
  fingerprint is ignored (the device had a stale copy). Otherwise the server
  asks the service itself: only its own "signed in" overrules the device.
  The checker asks every 6 h; only an explicit refusal (401, or 403
  `notLoggedInError`) marks it expired.
- Sign-out is everywhere: the device signs out at the service and deletes it
  here, or the website does both. One device alone cannot sign out while
  connected; the next sync would bring the session back.

### Website

- Follow a YouTube channel by link, @handle or UC id: a handle or link costs
  one page fetch (consent cookie `SOCS=CAE=`, redirects on the same host
  only) for the canonical `/channel/UC...`; the uploads feed then proves it
  exists and names it. Floatplane follows come from the account, Sauce+ picks
  from the app's catalog; the website lists them but does not add them.
- Thumbnails load from the hosts the app uses (CSP `img-src`).

### Feeds (0.2.0, decided 2026-10-24; app decision 19)

- **Lists only.** The server fetches what channels uploaded, never streams:
  googlevideo URLs are tied to the requesting IP, expire, and need the app's
  PoToken WebView; Floatplane's delivery endpoint is rate limited. Pressing
  play works as before.
- **Same lists as the devices make** (`core::feeds`, ported with the app's
  fixtures): YouTube's three uploads-playlist RSS feeds (`UULF`, `UUSH`,
  `UULV`; Shorts win a duplicate), the channel feed as fallback; Floatplane
  `creator/info` + `content/creator` (video posts in the plan only), the
  sub-channel list when needed, and the live check (`delivery/info` then the
  playlist's status, never with the cookie). No durations from RSS, as on
  the devices.
- **Owners**: YouTube rows belong to everyone (nil uuid): one fetch per
  channel for the house. Floatplane and Sauce+ rows are per person, fetched
  with their session, and skipped while it is expired; an explicit refusal
  marks it expired for all their devices.
- **Schedule** (`feed_channels.next_fetch_at`, one task, `Notify` + 30 s):
  15 min for favorites, 30 min by default, 3 h after 30 quiet days, 12 h after
  180, ±10 % jitter; failures back off 30 min doubling to 12 h; YouTube 429/403
  pauses all YouTube fetching for 1 h, Floatplane 429 its site for 15 min.
  Claims are leased 10 min. A follow wakes the task.
- **Storage**: a new `feed_rev` only when something shown changed; a known
  duration stays; a live item keeps its first time on air; the newest 200 per
  channel and kind (devices prune the same way); ended streams tombstoned for
  7 days; channels nobody follows dropped with their items after 7 days.
  About 1 KB per item; no files, so still no data volume.
- **`POST /api/v1/feed`**: the device names its channels with the highest rev
  it holds for each; no cursor on the server, so a new follow backfills. Only
  followed channels count. A rev beyond the sequence reads as 0. Up to 2000
  items per answer (`more`); per-channel status (`lastOkAt`, `lastError`,
  `rev`). Devices fetch a channel themselves when the server has no fresh
  status for it (2 h), so a down or disabled fetcher costs nothing.
- **`POST /api/v1/feed/refresh`**: pull-to-refresh. Channels not fetched in
  2 minutes become due, at most once a minute per person; waits up to 15 s.
- `SLOPSTREAM_FEEDS=false` turns the fetcher off.
- Rejected 2026-10-24: Valkey (Postgres covers queue, lock and wake-up) and a
  thumbnail cache (the CDNs and the devices' 250 MB caches do it; see roadmap).

### Long-poll and durations (0.3.0, 2026-10-24)

- **`GET /api/v1/changes?since=N`** answers at once when the person's counter
  is not N, else on the next change or after 25 s (`changed: false`). The open
  app waits on it instead of polling, so a follow on the website or a fresh
  list reaches it within seconds. Counters live in memory (`changes.rs`, one
  `watch` per person): the server is single-instance, so Postgres NOTIFY would
  add nothing; after a restart every device syncs once.
- **What bumps a person**: a sync that wrote rows; a website edit or follow;
  any session put, delete or status change; a stored list that changed for a
  channel they follow (YouTube: every follower; Floatplane, Sauce+: the
  owner); durations filled in. A sync that writes nothing wakes nobody.
- **Durations** (RSS has none): devices look them up from the channel's
  Videos and Live tabs (the app's NewPipeExtractor), because the server has no
  extractor and hand-written scraping would break with every YouTube change.
  The server hands them out: `/feed` answers with `durationJobs`, at most 3
  YouTube channels with new videos nobody checked, each leased to one device
  token for 10 minutes. `POST /api/v1/feed/durations` fills missing durations
  (a new rev each, so every device gets them) and marks the channel's
  unchecked videos checked, found or not: none is looked up twice. Shorts are
  skipped (YouTube lists no durations for them).
- Premieres (the tabs show them too) are parked: an "upcoming" state that ends
  at a time needs a schedule on both sides.

## Invariants

- **Writes for one person are serialized** (`library::lock`, an advisory lock
  per person) and take their rev inside it. Without that, a transaction that
  took rev 10 and committed after one with rev 11 would be skipped by a
  device whose cursor already passed 11.
- **The server is never behind a device's cursor silently.** A cursor beyond
  the `sync_rev` sequence (a restored backup, another server) gets `reset`:
  the device resends everything with base rev 0. Test
  `a_device_ahead_of_a_restored_server_is_asked_to_resend`.
- **Feeds never mix people's plans**: a Floatplane or Sauce+ item is only ever
  served to its owner (`floatplane_lists_are_per_person_...`), and a session
  cookie only goes to its service's API host.
- **A bad row refuses the whole request** (validation in `wire`), so a device
  never marks unsynced rows synced.
- **Rows are never hard-deleted**; `deleted_at` tombstones sync.
- **People never see each other's rows or sessions**: every query is scoped
  by the token's or the web session's person (`people_never_see_...`).
- **Session values are write-only** on the website, never logged, never in a
  URL; requests to the services carry them only in the Cookie header to that
  service's own host, with redirects off. reqwest errors are logged without
  URLs.
- Strict CSP: no inline script or style anywhere (the page smoke test checks
  every page). Writes from browsers need a same-origin `Origin`, plus
  `SameSite=Strict` cookies; referrer policy stays `same-origin`.
- Login answers the same for unknown, disabled, password-less and wrong
  password, and always pays one Argon2 verification. Five wrong pair codes
  end pairing for that browser session.
- The last active admin cannot demote or disable themselves.

## Roadmap

1. Long-poll `GET /api/v1/sync/wait` woken by Postgres NOTIFY, so website
   changes and fresh lists reach an open app at once instead of within a minute.
2. Optional thumbnail proxy for privacy (Google would not see which thumbnails
   a TV loads): a `/data` bind mount, files named by URL hash, a size cap with
   least-recently-used eviction, removed with their item, 30 days at most.
   About 1 GB for 12,000 items.
3. Sauce+ catalog and the Floatplane account's follows on the website (the
   server holds the sessions already).
4. NewPipe and Takeout imports on the website.
5. Synced app settings (quality cap, live toggle) per person.
6. `SLOPSTREAM_SECRET_KEY` rotation command.

## Deployment rules

- A new crate needs a `COPY crates/<name>/Cargo.toml` line in
  `docker/Dockerfile` (CI checks it).
- The release tag must equal `[workspace.package] version`.
- Gitea jobs never use `container:` with an image that lacks node.
- Compose files stay lean; reasons live here and in `deploy/README.md`.
- The owner does all git; never commit, tag or push unasked.

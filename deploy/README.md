# Deploying the Slopstream server

The released image `ghcr.io/xblackbytesx/slopstream-server` behind
Traefik, with Postgres 17. Tags: `X.Y.Z`, `X.Y`, `X`, `latest`.

## Files

Copy `docker-compose.yml` and `.env.example` (as `.env`) into your stack.

```sh
mkdir -p $DOCKER_ROOT/slopstream-server/database
```

Everything the server keeps lives in Postgres; the app has no volume.

## Configuration

| Variable | Required | Meaning |
|---|---|---|
| `DATABASE_URL` | yes | Postgres connection string |
| `SLOPSTREAM_SECRET_KEY` | yes | 32 random bytes, base64. Encrypts Floatplane and Sauce+ sessions at rest |
| `SLOPSTREAM_PUBLIC_URL` | no | The address devices use (`https://slopstream.example.com`), shown on a pairing TV. Default: the request's host |
| `SLOPSTREAM_FEEDS` | no | `false` stops fetching video lists here; devices then fetch their own (default on) |
| `SLOPSTREAM_BIND` | no | Listen address, default `0.0.0.0:8080` |
| `SLOPSTREAM_COOKIE_SECURE` | no | `false` only for plain-http LAN use (default on) |
| `RUST_LOG` | no | Log level, default `info` |

Generate the secret once and keep a copy with your backups:

```sh
head -c 32 /dev/urandom | base64
```

Without it, stored sessions cannot be decrypted and everyone signs in to
Floatplane and Sauce+ again. Changing it has the same effect. Libraries are
not affected.

## DNS and ports

- One host name, e.g. `slopstream.example.com`, routed by Traefik to port 8080.
  The app and the website use the same name.
- The router uses the `https-redirect`, `non-www`, `secure-headers` and
  `gzip-compress` file middlewares. The server sets its own
  Content-Security-Policy; if `secure-headers` also sets one, the stricter
  combination applies and must still allow `script-src 'self'`,
  `connect-src 'self'` and thumbnails from `i.ytimg.com`, `yt3.ggpht.com`,
  `yt3.googleusercontent.com` and `pbs.floatplane.com`.
- Postgres stays on the internal network.
- Open apps hold `GET /api/v1/changes` for up to 25 seconds (long-poll). Any
  proxy timeout above 30 seconds is fine (Traefik's and Nginx Proxy Manager's
  defaults are).
- Outbound: `www.youtube.com` RSS feeds, three per followed channel about
  every 30 minutes (200 channels: under one request a second, spread out);
  `www.floatplane.com` and `www.sauceplus.com` for followed creators' lists
  (one person at a time, a second apart), session checks every 6 hours and
  sign-out; Floatplane's live CDN for the on-air check. If Cloudflare
  challenges the server, sessions are stored as "not checked" and the devices
  keep using and fetching them.

## First run

```sh
docker compose up -d
docker compose exec slopstream-server-app slopstream-server create-admin you@example.com
docker compose exec -T slopstream-server-app slopstream-server set-password you@example.com
```

`set-password` reads one line from stdin. Then log in and:

1. Admin: invite everyone in the house with one-time links. Each person gets
   their own library and their own service sign-ins.
2. In the app on each device: Settings, Sync server, enter the address. The
   device shows a code; type it on the website under Devices, Pair.

## Upgrades

Migrations run at start. Pin `SLOPSTREAM_VERSION` to a release, pull, restart.

## Backups

Back up the Postgres volume plus `SLOPSTREAM_SECRET_KEY`. Restoring an older
backup is safe: on their next sync, devices see that the server is behind and
resend their library. The server stays leading for every row it still has.

## Verification

```sh
curl -fsS https://slopstream.example.com/healthz          # ok
curl -sI https://slopstream.example.com/login | grep -i content-security-policy
docker compose logs -f slopstream-server-app              # "listening"
```

A device token (Devices, Create token) can be checked with:

```sh
curl -fsS -H "Authorization: Bearer ss_..." https://slopstream.example.com/api/v1/me
```

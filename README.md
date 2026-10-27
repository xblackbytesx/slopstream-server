# Slopstream server

The optional home server for [Slopstream](https://github.com/xblackbytesx/slopstream), the
subscriptions-only video app for Android phones and TVs. One library per person, across all
their devices.

[![CI](https://github.com/xblackbytesx/slopstream-server/actions/workflows/ci.yml/badge.svg)](https://github.com/xblackbytesx/slopstream-server/actions/workflows/ci.yml)
[![Release](https://github.com/xblackbytesx/slopstream-server/actions/workflows/release.yml/badge.svg)](https://github.com/xblackbytesx/slopstream-server/actions/workflows/release.yml)
[![Latest release](https://img.shields.io/github/v/release/xblackbytesx/slopstream-server?sort=semver)](https://github.com/xblackbytesx/slopstream-server/releases/latest)
[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
[![Image](https://img.shields.io/badge/image-ghcr.io-2496ED?logo=docker&logoColor=white)](https://github.com/xblackbytesx/slopstream-server/pkgs/container/slopstream-server)
[![Rust 1.92](https://img.shields.io/badge/Rust-1.92-B7410E?logo=rust&logoColor=white)](rust-toolchain.toml)

Run it at home and every person in the house gets:

- subscriptions and favorites, Watch later, watch progress and history on every device;
- their own Floatplane and Sauce+ sign-ins: sign in once on any device, and when a session
  expires, sign in again on any one of them;
- a website to follow YouTube channels, tidy Watch later and history, and manage devices;
- the video lists of everything anyone follows, fetched all day (YouTube once for the whole
  house, Floatplane and Sauce+ per person), so devices open to a fresh grid; open apps hear
  about changes within seconds.

The server leads: when two devices disagree, its copy wins unless a change is clearly newer.

## What it is not

- Not a player. Streams are never fetched here: playback stays on the devices, and nothing
  plays on the website.
- No algorithm. It only fetches the channels people follow.
- Not shared. Each person has their own library and their own service sign-ins; sessions
  are encrypted at rest with `SLOPSTREAM_SECRET_KEY`.

## Quick start

```sh
cp .env.example .env        # set POSTGRES_PASSWORD and SLOPSTREAM_SECRET_KEY (make secret)
make up
make admin EMAIL=you@example.com
make password EMAIL=you@example.com
```

Open `http://localhost:8080`, log in, and invite the others under Admin.

## Deploy

The image is `ghcr.io/xblackbytesx/slopstream-server` (amd64 and arm64), tagged `X.Y.Z`,
`X.Y`, `X` and `latest`. A production example behind Traefik, with configuration, DNS,
backups and upgrades: [deploy/README.md](deploy/README.md).

## Pairing the app

Install [Slopstream](https://github.com/xblackbytesx/slopstream/releases/latest) (or add it
to Obtainium). In the app: Settings, Sync server, enter the server's address. The device
shows a code; type it on the website under Devices, Pair. Phones can also paste a token
instead.

Development and the design: [AGENTS.md](AGENTS.md).

## License

Slopstream server, the optional sync server for Slopstream.
Copyright (C) 2026 xblackbytesx

This program is free software: you can redistribute it and/or modify it under the terms of
the GNU General Public License as published by the Free Software Foundation, either version 3
of the License, or (at your option) any later version. See [LICENSE](LICENSE).

Not affiliated with YouTube, Google, Floatplane Media or Sauce+.

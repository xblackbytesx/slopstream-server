# Slopstream server

The optional companion to [Slopstream](../slopstream), the subscriptions-only
video app for phones and TVs. Run it at home and every person in the house
gets one library across their devices:

- subscriptions and favorites, Watch later, watch progress and history;
- their own Floatplane and Sauce+ sign-ins: sign in once on any device, and
  when a session expires, sign in again on any one of them;
- a website to follow YouTube channels, tidy Watch later and history, and
  manage devices;
- the video lists of everything anyone follows, fetched all day (YouTube once
  for the whole house, Floatplane and Sauce+ per person), so devices open to a
  fresh grid; open apps hear about changes within seconds.

Streams are never fetched here: playback stays on the devices.

The server leads: when two devices disagree, its copy wins unless a change is
clearly newer. Nothing plays on the website.

## Quick start

```sh
cp .env.example .env        # set POSTGRES_PASSWORD and SLOPSTREAM_SECRET_KEY (make secret)
make up
make admin EMAIL=you@example.com
make password EMAIL=you@example.com
```

Open `http://localhost:8080`, log in, invite the others under Admin. In the
app: Settings, Sync server, enter the address, and type the code the device
shows under Devices, Pair.

Deploying behind Traefik: [deploy/README.md](deploy/README.md). Development and
the design: [AGENTS.md](AGENTS.md).

## License

GPL-3.0-or-later, like the app.

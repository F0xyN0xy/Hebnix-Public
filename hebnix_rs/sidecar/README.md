# Workshop multiplayer sidecar

Workshop multiplayer runs through `hebnix-tc.exe`, a small Go program built
on [tailcat](https://github.com/tailscale/tailcat) (userspace WireGuard, from
the Tailscale team). It needs no network adapter, no Windows service, no
driver and no administrator rights.

- `hebnix-tc/`: the sidecar's source. The built `hebnix-tc.exe` is checked in
  at `crates/hebnix-app/assets/multiplayer/` and embedded in Hebnix, which
  extracts it to `%AppData%\Hebnix\multiplayer-lan` on start.
- `directory/`: the small web service every sidecar announces itself to so
  players find each other (no join codes). It runs on the multiplayer server;
  see its README.

The same source lives in the hebnix-linux repository; keep the two in step
(the wire protocol between sidecars is tailcat's, and tailcat makes no
compatibility promises between versions, so every player needs the same
build).

## How it works

Every player's sidecar runs a tailcat server, announces its address to the
directory, and connects to every other player listed there. Each other
player gets a loopback alias on this PC (127.10.0.n):

- LAN discovery: the sidecar shares Rocket League's discovery port 14001
  (SO_REUSEADDR), relays RL's LAN search and the hosting RL's answer, and
  rewrites the answer's `"ServerID"` to the host's alias, so RL's own LAN
  browser lists the match.
- Game: RL joins alias:7777, which is proxied to the host's own RL.
- Map sync: alias:14790 reaches that player's map sync.

Rocket League is started without `-multihome`, so its online play keeps
working while connected. Matches are kept private with the in-game LAN match
password. Hebnix talks to the sidecar through JSON lines on stdout; closing
its stdin (or Hebnix exiting) stops it and drops every connection.

## Rebuilding

Go 1.27+, from `hebnix-tc/`:

```
GOOS=windows GOARCH=amd64 CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o ../../crates/hebnix-app/assets/multiplayer/hebnix-tc.exe .
```

`go test ./...` runs the LAN-response rewrite tests. Bump the tailcat version
in `go.mod` only together with the Linux build.

## Directory key

Builds bake in the directory's shared key from `directory_key.txt` at the
repository root (gitignored) or the `HEBNIX_DIRECTORY_KEY` environment
variable in CI. `HEBNIX_DIRECTORY_URL` at runtime points Hebnix at a test
directory instead of `https://mp.hebnix.com/directory`.

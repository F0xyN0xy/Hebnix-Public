# Workshop multiplayer directory

A tiny service that lets every Hebnix with Workshop multiplayer on find
everyone else, so there are no join codes. It replaces headscale: Hebnix no
longer joins a tailnet, it runs `hebnix-tc` (tailcat, see `../hebnix-tc`) and
connects straight to the other players' sidecars.

Each Hebnix POSTs `{"id", "token", "name"}` every 20 s with the header
`X-Hebnix-Key`, and gets back everyone announced in the last 60 s. The token
is that player's tailcat address. Nothing is stored on disk; restarting the
service just means everyone reappears within 20 s. Stopping it stops new
connections; players already connected stay connected until they disconnect.

Python 3 standard library only.

## Setup

1. Pick a long random key and put it in `/etc/hebnix-directory.env`
   (mode 600):

   ```
   HEBNIX_DIRECTORY_KEY=<key>
   ```

   The same key is baked into Hebnix builds (`HEBNIX_DIRECTORY_KEY` in CI,
   or `directory_key.txt` at the hebnix-linux repository root for local
   builds).

2. Install and start it:

   ```
   sudo install -Dm755 directory.py /usr/local/lib/hebnix-directory/directory.py
   sudo install -Dm644 hebnix-directory.service /etc/systemd/system/hebnix-directory.service
   sudo systemctl daemon-reload
   sudo systemctl enable --now hebnix-directory
   ```

   It listens on `127.0.0.1:8087` (`HEBNIX_DIRECTORY_PORT` to change).

3. Put it behind the existing HTTPS reverse proxy. Caddy, next to the current
   headscale site:

   ```
   mp.hebnix.com {
   	handle /directory {
   		reverse_proxy 127.0.0.1:8087
   	}
   	# ...existing headscale config
   }
   ```

   nginx: `location = /directory { proxy_pass http://127.0.0.1:8087; proxy_set_header X-Forwarded-For $remote_addr; }`

Hebnix calls `https://mp.hebnix.com/directory` (`DIRECTORY_URL` in
`crates/hebnix-app/src/multiplayer_lan/mod.rs`). Once every player is on a
build with this, headscale can be shut down.

## Check it

```
curl -s https://mp.hebnix.com/directory -H "X-Hebnix-Key: <key>" \
  -d '{"id":"0123456789abcdef","token":"tcAAAAAAAAAAAAAAAAAAAAAAAA","name":"test"}'
```

returns `{"peers": [...]}` with the test entry in it (gone again after 60 s).

## Limits

- 60 announces per minute per client IP (Hebnix sends 3, so about 20 players
  behind one NAT), 1000 players at once.
- An X-Forwarded-For header is trusted for the client IP, so only expose it
  through the reverse proxy (it binds 127.0.0.1).

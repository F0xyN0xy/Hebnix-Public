# Proxy lifecycle and recovery

HTTP/WS interception uses the existing local listeners and hosts redirects. It
does not set Windows proxy settings or start the Hebnix UI with Windows.

- A background worker checks Rocket League every 250 ms. Listeners remain ready
  while the game is closed; hosts redirects and response modifications pause.
  Opening Rocket League publishes redirects again without rebuilding listeners.
- Enabling proxy features installs and verifies the current user certificate
  when needed. Removing it disables the spoofer master, individual spoof
  features, owned-item filtering, RLAPI capture, and automatic replay capture.
  HTTP/WS checkbox choices and saved spoof values are preserved. In-flight
  startup cannot republish redirects after certificate removal. External trust
  removal is checked approximately every five seconds.
- Every dependency (certificate, CRL server, WebSocket bridge, listener, cleanup
  watchdog, recovery tasks) must start successfully before hosts are redirected.
- A detached watchdog acknowledges a real parent process handle before startup
  succeeds. It waits in 250 ms intervals and retries cleanup after parent exit.
  If the watchdog is killed, the lifecycle worker pauses redirects and replaces
  it. Graceful UI exit removes redirects immediately and keeps the existing
  relay available for cached game connections until Rocket League exits.
- Cleanup-only scheduled tasks remove redirects at boot and clear dirty game
  WebCache at user logon. Each event runs cleanup once and exits. There are no
  timer triggers, repetitions, or scheduled retry loops. Hebnix prepares these
  fallback tasks when proxy setup needs recovery protection. The SYSTEM task
  executes a fixed script through Windows
  PowerShell; it never runs a Hebnix binary or a user-supplied script. Registry
  recovery identity contains only PID and creation time, checked under the same
  hosts mutex so a previous recovery task cannot clear a replacement session.
  The user cache task uses S4U on a non-interactive desktop, preventing visible
  PowerShell windows rather than relying on PowerShell to hide its own console.
- Hosts publication replaces a complete file atomically and preserves Windows
  file metadata. Cleanup preserves unrelated entries, including legacy markers.
  A dirty session marker remains until WebCache cleanup succeeds. Documents is
  resolved through its Windows known folder, including redirected Documents.
- Windows' configured DNS resolves upstream addresses without consulting hosts
  or its cache. Public DNS is a fallback. Warm listeners refresh addresses with
  a cache capped at 60 seconds. Startup and shutdown release listener ports
  before reuse, and incoming handshakes have timeouts.

## Limits

Hosts redirects remain system-wide while Rocket League is running. Another Epic
game using the same account hostname can still encounter certificate pinning or
intercepted responses during that time. Fully per-application interception would
require another transport architecture, such as a WFP callout driver.

A terminated process cannot preserve an existing authenticated game WebSocket.
The watchdog restores routing for new connections; Rocket League may need a
restart to discard its existing cached local connection. If both Hebnix and its
watchdog are terminated together, the fallback runs at boot/logon or Hebnix's
next startup; there is no periodic task running while Hebnix is closed.
Nothing can perform cleanup during a
power loss: the boot/logon tasks perform recovery afterwards. Windows permission,
Task Scheduler, antivirus, and networking failures can delay or prevent recovery.
S4U cannot access network shares or EFS-encrypted files. If Documents or the
recovery marker is stored there, unattended cache cleanup can fail; the normal
watchdog and Hebnix startup still retry in the user's existing session.

## Verification

Automated tests validate task definitions with Windows Task Scheduler without
registering them, parse the PowerShell script, execute cleanup against a temporary
hosts fixture, verify atomic publication, preserve foreign entries/newlines, and
exercise response modifications through disable, game-close, and resume states.

```powershell
cargo test -p hebnix-app --bin hebnix --offline
cargo test -p hebnix-app --example proxy_recovery_tests --offline
```

Before shipping, exercise these cases on a disposable Windows VM with real RL:

| Case | Expected result |
| --- | --- |
| Enable proxy without certificate | Certificate verified; setup failure leaves redirects absent |
| Close RL with Hebnix hidden | Redirects removed; settings retained; other Epic games reconnect |
| Reopen RL | Warm proxy resumes before authentication; spoof features still selected |
| Kill Hebnix | Detached watchdog removes redirects; cached connection may require RL restart |
| Kill the full process tree | Boot/logon or Hebnix's next startup restores connectivity |
| Power off with proxy active | Boot removes redirects; logon clears dirty WebCache without opening Hebnix |
| Remove certificate during proxy startup | Consumers off, HTTP/WS choices preserved, no redirects republished |
| Block ports 443, 8081, or 8025 | Startup fails before interception; no dead-end hosts mapping |
| Deny hosts writes or disable Task Scheduler | Setup reports failure; proxy does not publish redirects |
| Rapid disable/enable and RL restart | No listener-port race, duplicate redirects, or stale cached config |

# RLAPI plugin API

Plugins use Rocket League's captured WebSocket connection. Enable WebSockets
or RLAPI in Hebnix before launching Rocket League, then wait for its login to
be captured. The Experimental RLAPI tab and ballchasing uploads use this same
transport.

## `hebnix.rlapi.enabled`

A live, read-only boolean. It is `true` when WebSockets or RLAPI are enabled
and Rocket League's captured connection is available. It is `false` while
waiting for login, after the game disconnects, or when both features are disabled.
It does not depend on the separate Stats API connection.

## `hebnix.rlapi.matchhistory()`

Starts a background `Matches/GetMatchHistory v1` request for the local
`PlayerID` from `Launch.log`, using the same endpoint and payload as automatic
replay uploads. Call it again from `on_tick` to poll:

| Return values | Meaning |
| --- | --- |
| `nil, "pending"` | Request is running; poll again later. |
| `history, nil` | The full JSON response decoded into a Lua table. |
| `nil, error` | Unavailable connection, missing PlayerID, or request failure. |

Each plugin has one pending request. Polls reuse that request. Reading a
completed result consumes it; the next call starts a fresh request. Store the
returned table and stop polling until you want to refresh. Results from a
previous captured login are discarded if Rocket League reconnects.

```lua
local history
local finished = false

return {
    on_tick = function()
        if finished or not hebnix.rlapi.enabled then return end
        local result, state = hebnix.rlapi.matchhistory()
        if result then
            history = result
            finished = true
            hebnix.log(hebnix.json_encode(history))
        elseif state ~= "pending" then
            hebnix.log("Match history failed: " .. state)
            finished = true
        end
    end,
}
```

The old EOS token functions and flat `rlapi_request_async`, `rlapi_result`,
and `rlapi_connected` functions have been removed. Update plugins to use this
namespace. See the [RLAPI demo](../hebnix_rs/examples/plugins/rlapi_demo/main.lua)
for a refreshable match history window.

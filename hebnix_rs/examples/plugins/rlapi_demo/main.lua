-- Enable WebSockets or RLAPI before launching Rocket League.
local history = nil
local status = "Waiting for the captured game session"
local requested = false

return {
    on_load = function()
        hebnix.window.open{ title = "RLAPI Match History", width = 480, height = 360 }
    end,

    on_tick = function()
        if not hebnix.rlapi.enabled then
            status = "Enable WebSockets or RLAPI, then launch Rocket League"
            requested = false
            history = nil
            return
        end
        if requested then return end

        local result, state = hebnix.rlapi.matchhistory()
        if result then
            history = hebnix.json_encode(result)
            status = "Match history loaded"
            requested = true
        elseif state == "pending" then
            status = "Loading match history..."
        else
            status = state
            requested = true
        end
    end,

    on_window = function(ui)
        ui.heading("RLAPI Match History")
        ui.label("Available: " .. tostring(hebnix.rlapi.enabled))
        ui.label(status)
        if ui.button("Refresh") then
            requested = false
            history = nil
        end
        if history then
            ui.scroll_area(240, function(inner)
                inner.label(history)
            end)
        end
    end,
}

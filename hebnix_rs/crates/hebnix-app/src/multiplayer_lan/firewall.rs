//! Cleanup of the nftables rules older Hebnix versions added for Workshop
//! multiplayer (an `inet hebnix` table with allow rules for the tailnet and
//! a masquerade for `-multihome`). The tailcat sidecar needs none of them:
//! it only makes outbound connections and talks to Rocket League over
//! loopback. Without CAP_NET_ADMIN this quietly does nothing.

pub fn remove_rules() -> Result<(), String> {
    // dropping the whole table removes every rule we ever added in one shot
    let _ = super::caps::command_with_net_admin("nft")
        .args(["delete", "table", "inet", "hebnix"])
        .output();
    Ok(())
}

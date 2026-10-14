//! Backend regression tests without starting Hebnix or altering the installed game.
//! cargo test -p hebnix-app --example menu_map_tests
#![allow(dead_code, unused_imports)]
#[path = "../src/patcher/backup_guard.rs"]
pub mod backup_guard;
#[path = "../src/patcher/menu_map.rs"]
pub mod menu_map;
#[path = "../src/patcher/patch_core.rs"]
pub mod patch_core;
#[path = "../src/patcher/upk_keys.rs"]
pub mod upk_keys;
#[path = "../src/patcher/upk_package.rs"]
pub mod upk_package;
pub mod patcher {
    pub use crate::{backup_guard, patch_core, upk_package};
}
// Naming isn't part of the package/restore tests. The app uses Discord's shared
// arena labels; this harness avoids pulling the desktop UI into backend tests.
pub mod discord_presence {
    pub fn display_map_name(name: &str) -> String {
        name.to_string()
    }
}
fn main() {}

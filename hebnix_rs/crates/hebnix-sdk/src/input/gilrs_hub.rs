//! shared `Gilrs` instance, built once with the community GameControllerDB
//! (fresher than gilrs' own bundled snapshot). `xinput.rs` and `dinput.rs`
//! both read from this single handle instead of each opening their own onto
//! the same physical devices.

use std::sync::{Mutex, OnceLock};

use gilrs::{Gilrs, GilrsBuilder};

// gilrs 0.11's vendored SDL_GameControllerDB snapshot is missing newer
// controllers, so recognised-but-outdated devices fall back to gilrs' raw/
// positional layout instead of the real one. pulled fresh from
// https://github.com/mdqinc/SDL_GameControllerDB (zlib licensed) - update
// this file periodically for newly released pads.
const GAME_CONTROLLER_DB: &str = include_str!("../../assets/gamecontrollerdb.txt");

pub(super) fn handle() -> &'static Mutex<Option<Gilrs>> {
    static HANDLE: OnceLock<Mutex<Option<Gilrs>>> = OnceLock::new();
    HANDLE.get_or_init(|| {
        let built = GilrsBuilder::new()
            .add_included_mappings(false)
            .add_mappings(GAME_CONTROLLER_DB)
            .build();
        match built {
            Ok(g) => Mutex::new(Some(g)),
            Err(e) => {
                tracing::warn!("gilrs init failed, controller support disabled: {e}");
                Mutex::new(None)
            }
        }
    })
}

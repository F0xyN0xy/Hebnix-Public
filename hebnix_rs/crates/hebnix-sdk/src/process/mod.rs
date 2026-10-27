//! RL process detection + window helpers.

pub mod detector;
pub mod i3ipc;
pub mod window;
pub mod wine_prefix;
pub mod x11;

pub use detector::{
    RlPlatform, RlProcessInfo, detect_platform, find_process, find_rocket_league,
    get_save_data_path, is_rocket_league_running, save_data_dir_in,
};
pub use wine_prefix::{
    candidate_documents_dirs, documents_dir_for_exe, prefix_in_use, rocket_league_documents_dir,
    rocket_league_prefixes, running_game_prefixes,
    set_documents_override, steam_library_steamapps_dirs, wine_prefixes,
};
pub use window::{
    can_track_own_window, exempt_own_window_decorations, focus_own_window_over_game, get_rocket_league_window_rect,
    cursor_in_rl_window, cursor_position, hyprland_layer_visible, is_cursor_inside_rl_window,
    is_rocket_league_focused,
    raise_other_instance, reassert_own_window_decorations, rocket_league_hwnd,
    rocket_league_monitor_rect, rocket_league_monitor_size, set_own_always_on_top,
    unminimize_own_window,
};

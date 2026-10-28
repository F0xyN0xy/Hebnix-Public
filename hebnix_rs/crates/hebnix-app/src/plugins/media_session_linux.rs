//! whatever is "now playing" on the desktop, read from MPRIS players on the
//! session bus (Spotify, browsers, mpv, ...). linux-port: the Windows build
//! reads the system media transport controls instead. a worker thread polls
//! it and caches a snapshot so lua reads are just a mutex lock. it only
//! reports while rocket league is running

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gio::prelude::*;
use glib::{Variant, VariantDict, VariantTy};

const POLL: Duration = Duration::from_millis(750);
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const CALL_TIMEOUT_MS: i32 = 500;
// covers bigger than this are almost certainly junk
const MAX_THUMB: u64 = 16 * 1024 * 1024;

#[derive(Clone, Default)]
pub struct MediaSnapshot {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub app: String,
    pub status: &'static str,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub updated_unix_ms: i64,
    pub thumb_rev: u64,
}

#[derive(Default)]
struct Shared {
    snap: Option<MediaSnapshot>,
    thumb: Option<Vec<u8>>,
}

fn shared() -> &'static Mutex<Shared> {
    static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();
    SHARED.get_or_init(|| {
        std::thread::Builder::new()
            .name("media-session".into())
            .spawn(worker)
            .ok();
        Mutex::new(Shared::default())
    })
}

/// latest now-playing info, None when nothing has a media session
pub fn snapshot() -> Option<MediaSnapshot> {
    shared().lock().ok()?.snap.clone()
}

/// raw cover bytes for the current snapshot's thumb_rev, None until one loads
pub fn thumbnail() -> Option<Vec<u8>> {
    shared().lock().ok()?.thumb.clone()
}

fn status_name(s: &str) -> &'static str {
    match s {
        "Playing" => "playing",
        "Paused" => "paused",
        "Stopped" => "stopped",
        _ => "closed",
    }
}

fn call(
    conn: &gio::DBusConnection,
    dest: &str,
    path: &str,
    iface: &str,
    method: &str,
    args: Option<&Variant>,
) -> Option<Variant> {
    conn.call_sync(
        Some(dest),
        path,
        iface,
        method,
        args,
        None,
        gio::DBusCallFlags::NONE,
        CALL_TIMEOUT_MS,
        None::<&gio::Cancellable>,
    )
    .ok()
}

fn player_names(conn: &gio::DBusConnection) -> Vec<String> {
    call(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "ListNames",
        None,
    )
    .and_then(|reply| reply.child_value(0).get::<Vec<String>>())
    .unwrap_or_default()
    .into_iter()
    .filter(|name| name.starts_with(MPRIS_PREFIX))
    .collect()
}

/// the Player interface's properties, as a dict
fn player_props(conn: &gio::DBusConnection, name: &str) -> Option<VariantDict> {
    let args = (PLAYER_IFACE,).to_variant();
    let reply = call(
        conn,
        name,
        MPRIS_PATH,
        "org.freedesktop.DBus.Properties",
        "GetAll",
        Some(&args),
    )?;
    Some(VariantDict::new(Some(&reply.child_value(0))))
}

fn string_of(dict: &VariantDict, key: &str) -> String {
    dict.lookup_value(key, Some(VariantTy::STRING))
        .and_then(|v| v.str().map(str::to_string))
        .unwrap_or_default()
}

/// xesam:artist and friends are string arrays, a few players send a plain string
fn strings_of(dict: &VariantDict, key: &str) -> String {
    let Some(value) = dict.lookup_value(key, None) else {
        return String::new();
    };
    if let Some(list) = value.get::<Vec<String>>() {
        return list.join(", ");
    }
    value.str().map(str::to_string).unwrap_or_default()
}

/// mpris times are microseconds, as x or (from some players) t
fn micros_of(dict: &VariantDict, key: &str) -> i64 {
    let Some(value) = dict.lookup_value(key, None) else {
        return 0;
    };
    value
        .get::<i64>()
        .or_else(|| value.get::<u64>().map(|v| v.min(i64::MAX as u64) as i64))
        .unwrap_or(0)
}

/// "org.mpris.MediaPlayer2.firefox.instance_123" -> "firefox"
fn app_name(bus_name: &str) -> String {
    let rest = bus_name.strip_prefix(MPRIS_PREFIX).unwrap_or(bus_name);
    rest.split(".instance").next().unwrap_or(rest).to_string()
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

struct Read {
    snap: MediaSnapshot,
    key: String,
    art_url: String,
}

fn read_player(conn: &gio::DBusConnection, name: &str) -> Option<Read> {
    let props = player_props(conn, name)?;
    let metadata = VariantDict::new(props.lookup_value("Metadata", None).as_ref());
    let title = string_of(&metadata, "xesam:title");
    if title.is_empty() {
        return None;
    }
    let status = status_name(&string_of(&props, "PlaybackStatus"));
    let snap = MediaSnapshot {
        artist: strings_of(&metadata, "xesam:artist"),
        album: string_of(&metadata, "xesam:album"),
        album_artist: strings_of(&metadata, "xesam:albumArtist"),
        app: app_name(name),
        title,
        status,
        position_ms: micros_of(&props, "Position").max(0) / 1000,
        duration_ms: micros_of(&metadata, "mpris:length").max(0) / 1000,
        updated_unix_ms: now_unix_ms(),
        thumb_rev: 0,
    };
    let key = format!("{}\u{1}{}\u{1}{}", snap.app, snap.title, snap.artist);
    Some(Read {
        snap,
        key,
        art_url: string_of(&metadata, "mpris:artUrl"),
    })
}

/// the player to report: the first one playing, else the first paused one
fn current(conn: &gio::DBusConnection) -> Option<Read> {
    let mut fallback = None;
    for name in player_names(conn) {
        let Some(read) = read_player(conn, &name) else {
            continue;
        };
        if read.snap.status == "playing" {
            return Some(read);
        }
        if fallback.is_none() && read.snap.status == "paused" {
            fallback = Some(read);
        }
    }
    fallback
}

fn read_thumb_raw(art_url: &str) -> Option<Vec<u8>> {
    if let Some(path) = art_url.strip_prefix("file://") {
        let path = url::Url::parse(art_url)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .unwrap_or_else(|| path.into());
        let size = std::fs::metadata(&path).ok()?.len();
        if size == 0 || size > MAX_THUMB {
            return None;
        }
        return std::fs::read(path).ok();
    }
    if art_url.starts_with("http://") || art_url.starts_with("https://") {
        let response = ureq::get(art_url)
            .timeout(Duration::from_secs(5))
            .call()
            .ok()?;
        let mut buf = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(response.into_reader(), MAX_THUMB + 1),
            &mut buf,
        )
        .ok()?;
        if buf.is_empty() || buf.len() as u64 > MAX_THUMB {
            return None;
        }
        return Some(buf);
    }
    None
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

// youtube and friends hand over 16:9 frames, the overlay draws covers square,
// so center-crop rather than let them get squashed. undecodable bytes pass through
fn square_crop(bytes: Vec<u8>) -> Vec<u8> {
    let Ok(img) = image::load_from_memory(&bytes) else {
        return bytes;
    };
    let (w, h) = (img.width(), img.height());
    if w == h {
        return bytes;
    }
    let side = w.min(h);
    let cropped = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    let mut out = std::io::Cursor::new(Vec::new());
    match cropped.write_to(&mut out, image::ImageFormat::Png) {
        Ok(()) => out.into_inner(),
        Err(_) => bytes,
    }
}

fn worker() {
    let conn = loop {
        match gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) {
            Ok(conn) => break conn,
            Err(e) => {
                tracing::warn!("media session: no D-Bus session bus: {e}");
                std::thread::sleep(Duration::from_secs(10));
            }
        }
    };

    let mut last_key = String::new();
    let mut last_art = String::new();
    let mut thumb_rev = 0u64;
    let mut polls_since_track = 0u32;
    let mut thumb_hash = 0u64;
    loop {
        // only report media while rocket league is running, and don't touch
        // the bus at all otherwise
        if !hebnix_sdk::process::is_rocket_league_running() {
            last_key.clear();
            last_art.clear();
            polls_since_track = 0;
            thumb_hash = 0;
            if let Ok(mut sh) = shared().lock() {
                sh.snap = None;
                sh.thumb = None;
            }
            std::thread::sleep(POLL);
            continue;
        }
        // Some(None) drops the old cover, Some(Some(..)) swaps in a new one
        let mut new_thumb: Option<Option<Vec<u8>>> = None;
        let snap = current(&conn).map(|read| {
            let Read {
                mut snap,
                key,
                art_url,
            } = read;
            if key != last_key {
                last_key = key;
                polls_since_track = 0;
                thumb_rev += 1;
                new_thumb = Some(None);
            }
            // browsers change the title first and swap the cover in later,
            // so keep looking for the first ~30s of a track, then every few
            // polls. a remote cover is only fetched again when its url
            // changes; local files (browsers write them to /tmp) are re-read
            let check = polls_since_track < 40 || polls_since_track % 5 == 0;
            polls_since_track = polls_since_track.saturating_add(1);
            let remote = !art_url.starts_with("file://");
            if check && !art_url.is_empty() && (!remote || art_url != last_art || new_thumb.is_some()) {
                last_art = art_url.clone();
                if let Some(raw) = read_thumb_raw(&art_url) {
                    let hash = hash_bytes(&raw);
                    if hash != thumb_hash || new_thumb.is_some() {
                        thumb_hash = hash;
                        thumb_rev += 1;
                        new_thumb = Some(Some(square_crop(raw)));
                    }
                }
            }
            snap.thumb_rev = thumb_rev;
            snap
        });
        if snap.is_none() {
            last_key.clear();
            last_art.clear();
        }
        if let Ok(mut sh) = shared().lock() {
            if let Some(t) = new_thumb {
                sh.thumb = t;
            } else if snap.is_none() {
                sh.thumb = None;
            }
            sh.snap = snap;
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_name_strips_prefix_and_instance() {
        assert_eq!(app_name("org.mpris.MediaPlayer2.spotify"), "spotify");
        assert_eq!(
            app_name("org.mpris.MediaPlayer2.firefox.instance_1_42"),
            "firefox"
        );
    }

    #[test]
    fn reads_metadata_shapes() {
        let dict = VariantDict::new(None);
        dict.insert_value("xesam:title", &"Song".to_variant());
        dict.insert_value("xesam:artist", &vec!["A", "B"].to_variant());
        dict.insert_value("mpris:length", &123_000_000i64.to_variant());
        dict.insert_value("Position", &5_000_000u64.to_variant());
        assert_eq!(string_of(&dict, "xesam:title"), "Song");
        assert_eq!(strings_of(&dict, "xesam:artist"), "A, B");
        assert_eq!(micros_of(&dict, "mpris:length"), 123_000_000);
        assert_eq!(micros_of(&dict, "Position"), 5_000_000);
        assert_eq!(string_of(&dict, "missing"), "");
    }
}

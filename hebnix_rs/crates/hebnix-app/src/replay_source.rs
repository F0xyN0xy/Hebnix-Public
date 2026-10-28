//! Match history through the game's history export while running.
//! Separate account login is permitted only after the game exits.
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig};

#[cfg(target_os = "windows")]
#[path = "replay_process_memory.rs"]
mod memory;
#[cfg(target_os = "linux")]
#[path = "replay_process_memory_linux.rs"]
mod memory;

#[derive(Clone, Debug)]
struct BuildProfile {
    version: String,
    feature: String,
    build_id: i32,
    secret: String,
}

fn signature(body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(b"c338bd36fb8c42b1a431d30add939fc7").unwrap();
    mac.update(b"-");
    mac.update(body.as_bytes());
    STANDARD.encode(mac.finalize().into_bytes())
}

fn claims(token: &str) -> Option<Value> {
    let payload = token
        .strip_prefix("eg1~")
        .unwrap_or(token)
        .split('.')
        .nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?).ok()
}

fn token_matches(token: &str, account: &str, now: u64) -> bool {
    claims(token).is_some_and(|v| {
        v["sub"].as_str() == Some(account)
            && v["exp"].as_u64().is_some_and(|exp| exp > now + 30)
            && v.get("app").is_some()
    })
}

fn local_player() -> Option<String> {
    if let Some(player) =
        hebnix_sdk::log::local_player_id().filter(|id| !id.starts_with("Unknown|"))
    {
        return Some(player);
    }
    // A new or failed game launch can rotate Launch.log before writing a login.
    // Recover the last known local identity, which still must match the active
    // launcher token before any authenticated request is made.
    let logs =
        hebnix_sdk::utils::paths::documents_dir()?.join("My Games/Rocket League/TAGame/Logs");
    let mut backups: Vec<_> = std::fs::read_dir(logs)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("Launch-backup-")
                && entry.path().extension().is_some_and(|e| e == "log")
        })
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    backups.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    for (_, path) in backups.into_iter().take(3) {
        let bytes = std::fs::read(path).ok()?;
        if let Some(player) = backup_player(&String::from_utf8_lossy(&bytes)) {
            return Some(player);
        }
    }
    None
}

fn ensure_game_closed() -> Result<(), String> {
    if hebnix_sdk::process::is_rocket_league_running() {
        return Err("Replay recovery waits until Rocket League closes so a second account login cannot disconnect your game".into());
    }
    Ok(())
}

fn backup_player(text: &str) -> Option<String> {
    text.lines().rev().find_map(|line| {
        let player = line
            .split_once("Primary player ID? '")?
            .1
            .split_once('\'')?
            .0;
        (player.starts_with("Epic|") && player.ends_with("|0")).then(|| player.to_owned())
    })
}

fn api_json(response: reqwest::blocking::Response, stage: &str) -> Result<Value, String> {
    let status = response.status();
    let text = response
        .text()
        .map_err(|_| format!("{stage}: unreadable response ({status})"))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|_| format!("{stage}: invalid response ({status})"))?;
    if !status.is_success() || value.get("Error").is_some() || value.get("errorCode").is_some() {
        let code = value["Error"]["Type"]
            .as_str()
            .or_else(|| value["errorCode"].as_str())
            .unwrap_or("request rejected");
        return Err(format!("{stage}: {code} ({status})"));
    }
    Ok(value)
}

pub fn match_history(game_path: &Path, expected_player: Option<&str>) -> Result<Value, String> {
    if hebnix_sdk::process::is_rocket_league_running() {
        let player = hebnix_sdk::log::local_player_id()
            .filter(|p| !p.starts_with("Unknown|"))
            .ok_or("Rocket League has not identified its local account yet")?;
        if expected_player.is_some_and(|expected| expected != player) {
            return Err("Rocket League is signed into a different local account".into());
        }
        let log = hebnix_sdk::utils::paths::documents_dir()
            .ok_or("Could not find Rocket League's Documents directory")?
            .join("My Games/Rocket League/TAGame/Logs/Launch.log");
        let pid = memory::find_process("RocketLeague.exe").ok_or("Rocket League closed")?;
        let started =
            memory::started_at(pid).ok_or("Could not verify Rocket League's current process")?;
        let modified = std::fs::metadata(&log)
            .and_then(|m| m.modified())
            .map_err(|_| "Could not inspect current game log")?;
        if modified < started {
            return Err("Waiting for the current game to open its log".into());
        }
        return crate::replay_history_log::read(&log, &player);
    }
    let player = expected_player
        .map(str::to_owned)
        .or_else(local_player)
        .ok_or("Open Rocket League once so Hebnix can identify the local account")?;
    let account = player.strip_prefix("Epic|").and_then(|s| s.strip_suffix("|0"))
        .filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("Direct replay recovery currently requires an Epic account and its signed-in Epic Games Launcher")?;
    let profile = build_profile(game_path)?;
    let pid = memory::find_process("EpicGamesLauncher.exe").ok_or(
        "Sign into Epic Games Launcher with the same account as Rocket League, then retry",
    )?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let tokens = memory::scan_memory(pid, b"eg1~eyJ");
    let tokens: Vec<_> = tokens
        .into_iter()
        .filter(|t| token_matches(t, account, now))
        .collect();
    if tokens.is_empty() {
        return Err("No active launcher session matches Rocket League's local account; sign into that account in Epic Games Launcher".into());
    }
    let http = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(25))
        .build()
        .map_err(|e| e.to_string())?;
    let mut exchange = None;
    for token in tokens {
        let response = http
            .get(
                "https://account-public-service-prod03.ol.epicgames.com/account/api/oauth/exchange",
            )
            .bearer_auth(token)
            .send()
            .map_err(|_| "Could not reach Epic account services")?;
        if let Ok(value) = api_json(response, "Epic launcher session") {
            exchange = value["code"].as_str().map(str::to_owned);
            if exchange.is_some() {
                break;
            }
        }
    }
    let code = exchange
        .ok_or("Epic rejected the active launcher session; sign into Epic Games Launcher again")?;
    let eos = api_json(
        http.post("https://api.epicgames.dev/epic/oauth/v2/token")
            .basic_auth(
                "xyza7891p5D7s9R6Gm6moTHWGloerp7B",
                Some("Knh18du4NVlFs+3uQ+ZPpDCVto0WYf4yXP8+OcwVt1o"),
            )
            .form(&[
                ("grant_type", "exchange_code"),
                ("exchange_code", &code),
                ("deployment_id", "da32ae9c12ae40e8a112c52e1f17f3ba"),
                ("scope", "basic_profile"),
            ])
            .send()
            .map_err(|_| "Could not reach Epic authentication")?,
        "Epic authentication",
    )?;
    if eos["account_id"].as_str() != Some(account) {
        return Err("Epic returned a different account; replay request stopped".into());
    }
    let ticket = eos["access_token"]
        .as_str()
        .ok_or("Epic returned no access token")?;
    let body = json!({"Platform":"Epic", "PlayerName":"", "PlayerID":account,
        "Language":"INT", "AuthTicket":ticket, "BuildRegion":"", "FeatureSet":profile.feature,
        "Device":"PC", "LocalFirstPlayerID":player, "bSkipAuth":false,
        "bSetAsPrimaryAccount":true, "EpicAuthTicket":ticket, "EpicAccountID":account})
    .to_string();
    ensure_game_closed()?;
    let auth = api_json(
        http.post("https://api.rlpp.psynet.gg/rpc/Auth/AuthPlayer/v2")
            .header(
                "User-Agent",
                format!(
                    "RL Win/{} gzip (x86_64-pc-win32) curl-7.67.0 Schannel",
                    profile.version
                ),
            )
            .header("PsyBuildID", profile.build_id.to_string())
            .header("PsyEnvironment", "Prod")
            .header("PsyRequestID", "HebnixReplayAuth_0")
            .header("PsySig", signature(&body))
            .header("PsyBuildSecret", &profile.secret)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .map_err(|_| "Could not reach PsyNet authentication")?,
        "PsyNet authentication",
    )?;
    query_history(&profile, &player, &auth["Result"])
}

fn query_history(profile: &BuildProfile, player: &str, result: &Value) -> Result<Value, String> {
    let endpoint = reqwest::Url::parse(
        result["PerConURLv2"]
            .as_str()
            .ok_or("PsyNet returned no endpoint")?,
    )
    .map_err(|_| "PsyNet returned an invalid endpoint")?;
    if endpoint.scheme() != "wss"
        || endpoint.host_str() != Some("ws.rlpp.psynet.gg")
        || endpoint.port_or_known_default() != Some(443)
    {
        return Err("PsyNet returned an unexpected endpoint".into());
    }
    let mut request = endpoint
        .as_str()
        .into_client_request()
        .map_err(|e| e.to_string())?;
    for (key, value) in [
        ("PsyToken", result["PsyToken"].as_str().unwrap_or("")),
        ("PsySessionID", result["SessionID"].as_str().unwrap_or("")),
        ("PsyEnvironment", "Prod"),
    ] {
        if value.is_empty() {
            return Err("PsyNet returned an incomplete session".into());
        }
        request.headers_mut().insert(
            key,
            value.parse().map_err(|_| "Invalid PsyNet session header")?,
        );
    }
    request
        .headers_mut()
        .insert("PsyBuildID", profile.build_id.to_string().parse().unwrap());
    request.headers_mut().insert(
        "User-Agent",
        format!("RL Win/{} gzip", profile.version).parse().unwrap(),
    );
    let mut stream = None;
    for addr in ("ws.rlpp.psynet.gg", 443)
        .to_socket_addrs()
        .map_err(|_| "Could not resolve PsyNet")?
    {
        if let Ok(s) = TcpStream::connect_timeout(&addr, Duration::from_secs(10)) {
            stream = Some(s);
            break;
        }
    }
    let stream = stream.ok_or("Could not connect to PsyNet")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(20)))
        .map_err(|e| e.to_string())?;
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(8 * 1024 * 1024);
    config.max_frame_size = Some(8 * 1024 * 1024);
    let (mut socket, _) = tungstenite::client_tls_with_config(request, stream, Some(config), None)
        .map_err(|_| "Could not establish the direct PsyNet connection")?;
    let body = json!({"PlayerID":player}).to_string();
    let id = format!("HebnixReplay_{}", std::process::id());
    socket.send(Message::Text(format!("PsyService: Matches/GetMatchHistory v1\r\nPsyRequestID: {id}\r\nPsySig: {}\r\n\r\n{body}", signature(&body))))
        .map_err(|_| "Could not send match-history request")?;
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline {
        match socket
            .read()
            .map_err(|_| "Direct PsyNet connection closed or timed out")?
        {
            Message::Text(text) if text.starts_with("PsyPing:") => {
                let _ = socket.send(Message::Text("PsyPong:\r\n\r\n".into()));
            }
            Message::Text(text) => {
                let Some((head, body)) = text.split_once("\r\n\r\n") else {
                    continue;
                };
                if !head.lines().any(|line| {
                    line.split_once(':').is_some_and(|(k, v)| {
                        k.eq_ignore_ascii_case("PsyResponseID") && v.trim() == id
                    })
                }) {
                    continue;
                }
                let value: Value =
                    serde_json::from_str(body).map_err(|_| "Invalid match-history response")?;
                let _ = socket.close(None);
                if let Some(error) = value.get("Error") {
                    return Err(format!(
                        "Match history: {}",
                        error["Type"].as_str().unwrap_or("request rejected")
                    ));
                }
                let mut result = value
                    .get("Result")
                    .cloned()
                    .ok_or("PsyNet returned no match history")?;
                if let Some(object) = result.as_object_mut() {
                    object.insert("LocalPlayerID".into(), player.into());
                }
                return Ok(result);
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    Err("Match-history request timed out".into())
}

fn build_profile(game_path: &Path) -> Result<BuildProfile, String> {
    type Cached = (PathBuf, SystemTime, BuildProfile);
    static CACHE: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();
    let path = game_path.join("Binaries/Win64/RocketLeague.exe");
    let modified = path
        .metadata()
        .and_then(|m| m.modified())
        .map_err(|_| "Set the Rocket League installation folder in Settings")?;
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "Replay build cache unavailable")?;
    if let Some((p, m, profile)) = cache.as_ref() {
        if p == &path && *m == modified {
            return Ok(profile.clone());
        }
    }
    let bytes =
        std::fs::read(&path).map_err(|_| "Could not read Rocket League's version information")?;
    let profile = parse_build(&bytes)?;
    *cache = Some((path, modified, profile.clone()));
    Ok(profile)
}

// Decode only PE metadata and the game's static XOR-obfuscated build constant.
// No hardcoded game offsets, injected code, or executable patching.
fn parse_build(bytes: &[u8]) -> Result<BuildProfile, String> {
    let invalid = || {
        "This Rocket League build's replay API profile could not be read; update Hebnix".to_string()
    };
    let u16_at = |o: usize| {
        bytes
            .get(o..o.checked_add(2)?)
            .map(|b| u16::from_le_bytes(b.try_into().unwrap()))
    };
    let u32_at = |o: usize| {
        bytes
            .get(o..o.checked_add(4)?)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    let u64_at = |o: usize| {
        bytes
            .get(o..o.checked_add(8)?)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    };
    let pe = u32_at(0x3c).ok_or_else(invalid)? as usize;
    if bytes.get(pe..pe + 4) != Some(b"PE\0\0") || u16_at(pe + 24) != Some(0x20b) {
        return Err(invalid());
    }
    let base = u64_at(pe + 48).ok_or_else(invalid)?;
    let section_start = pe + 24 + u16_at(pe + 20).ok_or_else(invalid)? as usize;
    let count = u16_at(pe + 6).ok_or_else(invalid)? as usize;
    let mut sections = Vec::new();
    for i in 0..count.min(96) {
        let o = section_start + i * 40;
        sections.push((
            u32_at(o + 12).ok_or_else(invalid)? as u64,
            u32_at(o + 20).ok_or_else(invalid)? as usize,
            u32_at(o + 16).ok_or_else(invalid)? as usize,
        ));
    }
    let offset = |va: u64| -> Option<usize> {
        let rva = va.checked_sub(base)?;
        sections.iter().find_map(|(v, raw, size)| {
            if rva >= *v && rva - *v < *size as u64 {
                raw.checked_add((rva - *v) as usize)
                    .filter(|o| *o < bytes.len())
            } else {
                None
            }
        })
    };
    let address = |o: usize| -> Option<u64> {
        sections.iter().find_map(|(v, raw, size)| {
            if o >= *raw && o - *raw < *size {
                Some(base + v + (o - raw) as u64)
            } else {
                None
            }
        })
    };
    let wide = |o: usize| -> Option<String> {
        let chars: Vec<u16> = bytes
            .get(o..)?
            .chunks_exact(2)
            .take(256)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|c| *c != 0)
            .collect();
        String::from_utf16(&chars).ok()
    };
    let export =
        offset(base + u32_at(pe + 24 + 112).ok_or_else(invalid)? as u64).ok_or_else(invalid)?;
    let names =
        offset(base + u32_at(export + 32).ok_or_else(invalid)? as u64).ok_or_else(invalid)?;
    let functions =
        offset(base + u32_at(export + 28).ok_or_else(invalid)? as u64).ok_or_else(invalid)?;
    let ordinals =
        offset(base + u32_at(export + 36).ok_or_else(invalid)? as u64).ok_or_else(invalid)?;
    let mut version = None;
    for i in 0..(u32_at(export + 24).ok_or_else(invalid)? as usize).min(65536) {
        let n =
            offset(base + u32_at(names + i * 4).ok_or_else(invalid)? as u64).ok_or_else(invalid)?;
        if bytes.get(n..n + 16) == Some(b"GPsyonixBuildID\0") {
            let ordinal = u16_at(ordinals + i * 2).ok_or_else(invalid)? as usize;
            let pointer =
                offset(base + u32_at(functions + ordinal * 4).ok_or_else(invalid)? as u64)
                    .ok_or_else(invalid)?;
            version = wide(offset(u64_at(pointer).ok_or_else(invalid)?).ok_or_else(invalid)?);
            break;
        }
    }
    let version = version
        .filter(|s| {
            !s.is_empty() && s.len() < 64 && s.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        })
        .ok_or_else(invalid)?;
    let needle: Vec<u8> = "PrimeUpdate"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut features = Vec::new();
    for (i, w) in bytes.windows(needle.len()).enumerate() {
        if w != needle {
            continue;
        }
        if let Some(value) = wide(i) {
            let suffix = &value[11..];
            let mut parts = suffix.split('_');
            if let Ok(major) = parts.next().unwrap_or("").parse::<u32>() {
                let minor = match parts.next() {
                    Some(s) => match s.parse::<u32>() {
                        Ok(n) => n,
                        Err(_) => continue,
                    },
                    None => 0,
                };
                if parts.next().is_none() {
                    features.push((major, minor, value));
                }
            }
        }
    }
    let feature = features.into_iter().max().ok_or_else(invalid)?.2;
    let mut secret = None;
    for (i, w) in bytes.windows(14).enumerate() {
        if w[..3] != [0x4c, 0x8d, 0x05] || w[7..10] != [0x4c, 0x8d, 0x0d] {
            continue;
        }
        let Some(va) = address(i) else {
            continue;
        };
        let start = (va as i64 + 7 + i32::from_le_bytes(w[3..7].try_into().unwrap()) as i64) as u64;
        let end =
            (va as i64 + 14 + i32::from_le_bytes(w[10..14].try_into().unwrap()) as i64) as u64;
        if end.checked_sub(start) != Some(64) {
            continue;
        }
        let Some(o) = offset(start) else {
            continue;
        };
        let Some(data) = bytes.get(o..o + 68) else {
            continue;
        };
        let decoded: Vec<u8> = (0..64).map(|n| data[n] ^ data[64 + n % 4]).collect();
        if decoded.iter().all(u8::is_ascii_hexdigit) {
            let value = String::from_utf8(decoded).unwrap();
            if secret.as_ref().is_some_and(|old| old != &value) {
                return Err(invalid());
            }
            secret = Some(value);
        }
    }
    let secret = secret.ok_or_else(invalid)?;
    let mut crc = u32::MAX;
    for b in version.encode_utf16().flat_map(u16::to_le_bytes) {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    Ok(BuildProfile {
        version,
        feature,
        build_id: (crc ^ u32::MAX) as i32,
        secret,
    })
}

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{ReplayUploadCfg, RlLaunchCfg, RlLaunchMode};
use crate::messages::AppMsg;

const HISTORY_FILE: &str = "replay_upload_history.json";
use hebnix_sdk::rlapi::session::MATCH_HISTORY_SERVICE;
const POLL_INTERVAL: Duration = Duration::from_secs(30);
const POLL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_REPLAY_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadHistoryEntry {
    pub match_guid: String,
    pub replay_id: String,
    pub location: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub warning: String,
    #[serde(default)]
    pub duplicate: bool,
    #[serde(default)]
    pub local_path: String,
}

#[derive(Debug)]
pub struct UploadResult {
    pub match_guid: String,
    pub result: Result<UploadHistoryEntry, String>,
}

#[derive(Debug, Clone)]
struct TrackedMatch {
    match_guid: String,
    player_id: String,
}

pub struct AutoUploadReplays {
    base_dir: PathBuf,
    running: bool,
    current_match: Option<TrackedMatch>,
    submitted: HashSet<String>,
    pending: HashSet<String>,
    history: Vec<UploadHistoryEntry>,
    status: String,
    generation: Arc<AtomicU64>,
}

impl AutoUploadReplays {
    pub fn new(base_dir: &Path) -> Self {
        let history = std::fs::read(base_dir.join(HISTORY_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<UploadHistoryEntry>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .take(20)
            .collect();
        Self {
            base_dir: base_dir.to_path_buf(),
            running: false,
            current_match: None,
            submitted: HashSet::new(),
            pending: HashSet::new(),
            history,
            status: "Disabled".to_string(),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn running(&self) -> bool {
        self.running
    }

    pub fn sync(&mut self, enabled: bool, game_running: bool, capture_enabled: bool) {
        if enabled && game_running && capture_enabled {
            if !self.running {
                self.running = true;
                self.status = "Running · waiting for a match".to_string();
            }
        } else {
            if self.running {
                self.stop();
            }
            self.status = if !enabled {
                "Disabled"
            } else if !game_running {
                "Waiting for Rocket League"
            } else {
                "Waiting for the proxy · administrator access and a trusted certificate are required"
            }.to_string();
        }
    }

    pub fn stop(&mut self) {
        self.running = false;
        self.current_match = None;
        self.pending.clear();
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.status = "Stopped".to_string();
    }

    pub fn needs_match_identity(&self, match_guid: Option<&str>) -> bool {
        self.running
            && match_guid
                .filter(|guid| !guid.trim().is_empty())
                .is_some_and(|guid| {
                    let guid = normalize_guid(guid);
                    !self.submitted.contains(&guid)
                        && self
                            .current_match
                            .as_ref()
                            .is_none_or(|tracked| tracked.match_guid != guid)
                })
    }

    pub fn track_match(&mut self, match_guid: &str, player_id: Option<String>) {
        if !self.running || match_guid.trim().is_empty() {
            return;
        }
        let guid = normalize_guid(match_guid);
        if self.submitted.contains(&guid) {
            return;
        }
        let Some(player_id) = player_id.filter(|value| !value.trim().is_empty()) else {
            self.status = "Match detected, but the local PlayerID was not found".to_string();
            return;
        };
        if self
            .current_match
            .as_ref()
            .is_none_or(|tracked| tracked.match_guid != guid)
        {
            self.status = format!("Tracking match {guid}");
            self.current_match = Some(TrackedMatch {
                match_guid: guid,
                player_id,
            });
        }
    }

    pub fn submit_finished(
        &mut self,
        event_guid: Option<&str>,
        config: &ReplayUploadCfg,
        game_path: &Path,
        launch: &RlLaunchCfg,
        tx: Sender<AppMsg>,
        ctx: egui::Context,
    ) -> Option<String> {
        if !self.running {
            return None;
        }
        if !config.upload_enabled && !config.save_locally {
            self.status = "Select upload to ballchasing.com or save locally".to_string();
            return None;
        }
        let local_dir = if config.save_locally {
            match configured_replay_directory(config, game_path, launch) {
                Ok(path) => Some(path),
                Err(error) => {
                    self.status = error;
                    return None;
                }
            }
        } else {
            None
        };
        let tracked = self.current_match.as_ref()?;
        if let Some(event_guid) = event_guid {
            if normalize_guid(event_guid) != tracked.match_guid {
                return None;
            }
        }
        let tracked = self.current_match.take()?;
        if self.submitted.contains(&tracked.match_guid) {
            return None;
        }
        self.submitted.insert(tracked.match_guid.clone());
        self.pending.insert(tracked.match_guid.clone());
        self.status = format!("Waiting for replay {}", tracked.match_guid);

        let debug_message = config
            .debug_logging
            .then(|| format!("[Replay Upload] polling match {}", tracked.match_guid));
        let job = UploadJob {
            tracked,
            upload_enabled: config.upload_enabled,
            local_dir,
            api_key: config.api_key.trim().to_string(),
            naming_template: config.naming_template.trim().to_string(),
            visibility: config.visibility.clone(),
            group_id: config.group_id.trim().to_string(),
            generation: Arc::clone(&self.generation),
            expected_generation: self.generation.load(Ordering::Acquire),
        };
        std::thread::Builder::new()
            .name("replay-auto-upload".to_string())
            .spawn(move || {
                let match_guid = job.tracked.match_guid.clone();
                let result = run_upload(job);
                let _ = tx.send(AppMsg::ReplayUploadFinished(UploadResult {
                    match_guid,
                    result,
                }));
                ctx.request_repaint();
            })
            .ok();
        debug_message
    }

    pub fn finish_upload(&mut self, result: UploadResult) {
        // A stopped run invalidates its workers. Ignore their eventual wake-up
        // instead of replacing the visible "Stopped" state with a stale error.
        if !self.pending.remove(&result.match_guid) {
            return;
        }
        match result.result {
            Ok(entry) => {
                self.status = format!("{} replay {}", replay_outcome(&entry), entry.match_guid);
                self.history
                    .retain(|item| item.match_guid != entry.match_guid);
                self.history.insert(0, entry);
                self.history.truncate(20);
                self.save_history();
            }
            Err(error) => self.status = format!("Replay failed for {}: {error}", result.match_guid),
        }
    }

    fn save_history(&self) {
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.history) {
            let _ = std::fs::write(self.base_dir.join(HISTORY_FILE), bytes);
        }
    }

    pub fn show_status(&self, ui: &mut egui::Ui, session_status: &str) {
        ui.heading("Replays");
        ui.label("Automatically saves or uploads matches while either replay option is enabled.");
        ui.add_space(8.0);
        ui.label(&self.status);
        ui.small(session_status);
        if !self.pending.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("{} replay(s) pending", self.pending.len()));
            });
        }
    }

    pub fn show_history(&self, ui: &mut egui::Ui) {
        ui.heading("Last 20 replays");
        if self.history.is_empty() {
            ui.label("No replays saved or uploaded yet.");
        } else {
            egui::ScrollArea::vertical()
                .id_salt("replay_upload_history")
                .show(ui, |ui| {
                    for entry in &self.history {
                        ui.horizontal_wrapped(|ui| {
                            ui.monospace(&entry.match_guid);
                            ui.label(replay_outcome(entry));
                            if !entry.location.is_empty() {
                                ui.hyperlink_to("Open on ballchasing.com", &entry.location);
                            }
                        });
                        if !entry.local_path.is_empty() {
                            ui.small(format!("Saved to {}", entry.local_path));
                        }
                        if !entry.title.is_empty() {
                            ui.label(&entry.title);
                        }
                        if !entry.warning.is_empty() {
                            ui.colored_label(egui::Color32::YELLOW, &entry.warning);
                        }
                        ui.separator();
                    }
                });
        }
    }
}

struct UploadJob {
    tracked: TrackedMatch,
    upload_enabled: bool,
    local_dir: Option<PathBuf>,
    api_key: String,
    naming_template: String,
    visibility: String,
    group_id: String,
    generation: Arc<AtomicU64>,
    expected_generation: u64,
}

impl UploadJob {
    fn cancelled(&self) -> bool {
        self.generation.load(Ordering::Acquire) != self.expected_generation
    }
}

fn run_upload(job: UploadJob) -> Result<UploadHistoryEntry, String> {
    if job.upload_enabled && job.api_key.is_empty() && job.local_dir.is_none() {
        return Err("enter a ballchasing.com API key".to_string());
    }
    if !job.upload_enabled && job.local_dir.is_none() {
        return Err("select upload to ballchasing.com or save locally".to_string());
    }
    let deadline = Instant::now() + POLL_TIMEOUT;
    let mut first_attempt = true;
    loop {
        if job.cancelled() {
            return Err("stopped".to_string());
        }
        if !first_attempt && Instant::now() >= deadline {
            return Err("replay did not appear in match history within 5 minutes".to_string());
        }
        first_attempt = false;
        let attempt_started = Instant::now();
        let response = hebnix_sdk::rlapi::session::shared_game_session().request(
            MATCH_HISTORY_SERVICE,
            serde_json::json!({ "PlayerID": job.tracked.player_id }),
        );
        if let Ok(value) = response {
            if let Some(entry) = match_entry(&value, &job.tracked.match_guid) {
                let replay_url = entry
                    .get("ReplayUrl")
                    .and_then(Value::as_str)
                    .filter(|url| !url.is_empty())
                    .ok_or("match history entry did not include a replay URL")?;
                let title = render_replay_title(&job.naming_template, entry);
                return download_and_upload(&job, replay_url, &title);
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return Err("replay did not appear in match history within 5 minutes".to_string());
        }
        let next_attempt = (attempt_started + POLL_INTERVAL).min(deadline);
        std::thread::sleep(next_attempt.saturating_duration_since(now));
    }
}

fn match_entry<'a>(value: &'a Value, match_guid: &str) -> Option<&'a Value> {
    value.get("Matches")?.as_array()?.iter().find_map(|entry| {
        let guid = entry.get("Match")?.get("MatchGUID")?.as_str()?;
        if normalize_guid(guid) == normalize_guid(match_guid) {
            Some(entry)
        } else {
            None
        }
    })
}

fn download_and_upload(
    job: &UploadJob,
    replay_url: &str,
    title: &str,
) -> Result<UploadHistoryEntry, String> {
    let replay_url = validate_replay_url(replay_url)?;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(replay_url)
        .send()
        .map_err(|error| format!("could not download replay: {error}"))?
        .error_for_status()
        .map_err(|error| format!("could not download replay: {error}"))?;
    if response
        .content_length()
        .is_some_and(|size| size > MAX_REPLAY_BYTES)
    {
        return Err("replay download was unexpectedly large".to_string());
    }
    let replay = response
        .bytes()
        .map_err(|error| format!("could not read replay: {error}"))?;
    if replay.len() as u64 > MAX_REPLAY_BYTES {
        return Err("replay download was unexpectedly large".to_string());
    }
    if job.cancelled() {
        return Err("stopped".to_string());
    }

    deliver_replay(job, &replay, title, || {
        upload_replay(&client, job, &replay, title)
    })
}

// The local destination writes the downloaded bytes directly. It never calls
// Epic's replay upload service or the game's cloud-save APIs.
fn deliver_replay(
    job: &UploadJob,
    replay: &[u8],
    title: &str,
    upload: impl FnOnce() -> Result<UploadHistoryEntry, String>,
) -> Result<UploadHistoryEntry, String> {
    if job.cancelled() {
        return Err("stopped".to_string());
    }
    let local_result = job
        .local_dir
        .as_ref()
        .map(|directory| save_local_replay(directory, &job.tracked.match_guid, replay));
    let mut entry = UploadHistoryEntry {
        match_guid: job.tracked.match_guid.clone(),
        replay_id: String::new(),
        location: String::new(),
        title: title.to_string(),
        warning: String::new(),
        duplicate: false,
        local_path: String::new(),
    };
    if job.upload_enabled {
        if job.cancelled() {
            return Err("stopped".to_string());
        }
        match upload() {
            Ok(uploaded) => entry = uploaded,
            Err(error) if matches!(&local_result, Some(Ok(_))) => {
                entry.warning = format!("Replay saved locally, but upload failed: {error}");
            }
            Err(error) => return Err(error),
        }
    }
    match local_result {
        Some(Ok(path)) => entry.local_path = path.to_string_lossy().into_owned(),
        Some(Err(error)) if !entry.replay_id.is_empty() => {
            if !entry.warning.is_empty() {
                entry.warning.push('\n');
            }
            entry
                .warning
                .push_str(&format!("Replay uploaded, but local save failed: {error}"));
        }
        Some(Err(error)) => return Err(error),
        None => {}
    }
    Ok(entry)
}

fn upload_replay(
    client: &reqwest::blocking::Client,
    job: &UploadJob,
    replay: &[u8],
    title: &str,
) -> Result<UploadHistoryEntry, String> {
    let mut url = reqwest::Url::parse("https://ballchasing.com/api/v2/upload")
        .map_err(|error| error.to_string())?;
    url.query_pairs_mut()
        .append_pair("visibility", visibility(&job.visibility));
    if !job.group_id.is_empty() {
        url.query_pairs_mut().append_pair("group", &job.group_id);
    }
    let part = reqwest::blocking::multipart::Part::bytes(replay.to_vec())
        .file_name(format!("{}.replay", job.tracked.match_guid))
        .mime_str("application/octet-stream")
        .map_err(|error| error.to_string())?;
    let response = client
        .post(url)
        .header(reqwest::header::AUTHORIZATION, &job.api_key)
        .multipart(reqwest::blocking::multipart::Form::new().part("file", part))
        .send()
        .map_err(|error| format!("ballchasing upload failed: {error}"))?;
    let status = response.status();
    let response_text = response
        .text()
        .map_err(|error| format!("could not read ballchasing response ({status}): {error}"))?;
    let body: Value = serde_json::from_str(&response_text)
        .map_err(|error| format!("ballchasing returned invalid JSON ({status}): {error}"))?;
    if status.as_u16() != 201 && status.as_u16() != 409 {
        let detail = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("request rejected");
        return Err(format!("ballchasing returned {status}: {detail}"));
    }
    let replay_id = body
        .get("id")
        .and_then(Value::as_str)
        .ok_or("ballchasing response did not include a replay id")?
        .to_string();
    let location = body
        .get("location")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://ballchasing.com/replay/{replay_id}"));
    let warning = if title.is_empty() {
        String::new()
    } else {
        patch_replay_title(&client, &job.api_key, &replay_id, title)
            .err()
            .map(|error| format!("Replay uploaded, but its name could not be set: {error}"))
            .unwrap_or_default()
    };
    Ok(UploadHistoryEntry {
        match_guid: job.tracked.match_guid.clone(),
        replay_id,
        location,
        title: title.to_string(),
        warning,
        duplicate: status.as_u16() == 409,
        local_path: String::new(),
    })
}

pub fn replay_directory(game_path: &Path, launch: &RlLaunchCfg) -> Result<PathBuf, String> {
    use hebnix_sdk::process::{RlPlatform, detect_platform};
    let steam = match detect_platform(game_path) {
        RlPlatform::Steam => true,
        RlPlatform::Epic => false,
        RlPlatform::Unknown => launch.mode == RlLaunchMode::SteamNative,
    };
    let documents = dirs::document_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Documents")))
        .filter(|path| path.is_absolute())
        .ok_or("Documents folder is unavailable")?;
    Ok(replay_directory_in(&documents, steam))
}

pub fn configured_replay_directory(
    config: &ReplayUploadCfg,
    game_path: &Path,
    launch: &RlLaunchCfg,
) -> Result<PathBuf, String> {
    let directory = config.save_directory.trim();
    if directory.is_empty() {
        return replay_directory(game_path, launch);
    }
    let path = PathBuf::from(directory);
    if !path.is_absolute() {
        return Err("Choose an absolute replay save folder".into());
    }
    Ok(path)
}

fn replay_directory_in(documents: &Path, steam: bool) -> PathBuf {
    documents
        .join("My Games")
        .join("Rocket League")
        .join("TAGame")
        .join(if steam { "Demos" } else { "DemosEpic" })
}

fn save_local_replay(directory: &Path, match_guid: &str, replay: &[u8]) -> Result<PathBuf, String> {
    use std::io::Write;
    // Match history is remote data; never allow its GUID to become a path.
    if match_guid.is_empty()
        || match_guid.len() > 128
        || !match_guid
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("match history returned an invalid match GUID".to_string());
    }
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("could not create replay folder: {error}"))?;
    let path = directory.join(format!("{match_guid}.replay"));
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::read(&path).ok().as_deref() == Some(replay) {
                return Ok(path);
            }
            return Err(format!(
                "a different replay already exists at {}",
                path.display()
            ));
        }
        Err(error) => return Err(format!("could not save replay: {error}")),
    };
    if let Err(error) = file.write_all(replay).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(format!("could not save replay: {error}"));
    }
    Ok(path)
}

fn replay_outcome(entry: &UploadHistoryEntry) -> &'static str {
    match (
        !entry.local_path.is_empty(),
        !entry.replay_id.is_empty(),
        entry.duplicate,
    ) {
        (true, true, true) => "Saved locally · already uploaded",
        (true, true, false) => "Saved locally · uploaded",
        (true, false, _) => "Saved locally",
        (false, _, true) => "Already uploaded",
        _ => "Uploaded",
    }
}

fn patch_replay_title(
    client: &reqwest::blocking::Client,
    api_key: &str,
    replay_id: &str,
    title: &str,
) -> Result<(), String> {
    let response = client
        .patch(format!("https://ballchasing.com/api/replays/{replay_id}"))
        .header(reqwest::header::AUTHORIZATION, api_key)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "title": title }).to_string())
        .send()
        .map_err(|error| error.to_string())?;
    if response.status().as_u16() == 204 {
        Ok(())
    } else {
        Err(format!("ballchasing returned {}", response.status()))
    }
}

fn render_replay_title(template: &str, entry: &Value) -> String {
    let details = entry.get("Match").unwrap_or(&Value::Null);
    let timestamp = details
        .get("RecordStartTimestamp")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let winning_team = match details.get("WinningTeam").and_then(Value::as_i64) {
        Some(0) => "Blue",
        Some(1) => "Orange",
        _ => "Unknown",
    };
    let playlist = details
        .get("Playlist")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let match_guid = details
        .get("MatchGUID")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let map = details
        .get("MapName")
        .and_then(Value::as_str)
        .unwrap_or("Unknown Map");
    let (time, time24) = local_times(timestamp);
    template
        .replace("{date}", &unix_date(timestamp))
        .replace("{time24}", &time24)
        .replace("{time}", &time)
        .replace("{winning_team}", winning_team)
        .replace("{gamemode}", gamemode_name(playlist))
        .replace("{map}", map)
        .replace("{match_guid}", match_guid)
        .trim()
        .chars()
        .take(200)
        .collect()
}

fn local_times(timestamp: i64) -> (String, String) {
    let Ok(utc) = time::OffsetDateTime::from_unix_timestamp(timestamp) else {
        return ("12:00 AM".to_string(), "00:00".to_string());
    };
    let offset = time::UtcOffset::local_offset_at(utc).unwrap_or(time::UtcOffset::UTC);
    let local = utc.to_offset(offset);
    let hour24 = local.hour();
    let minute = local.minute();
    let suffix = if hour24 < 12 { "AM" } else { "PM" };
    let hour12 = match hour24 % 12 {
        0 => 12,
        hour => hour,
    };
    (
        format!("{hour12}:{minute:02} {suffix}"),
        format!("{hour24:02}:{minute:02}"),
    )
}

fn gamemode_name(playlist: i64) -> &'static str {
    match playlist {
        1 | 10 => "1v1",
        2 | 11 => "2v2",
        3 | 13 => "3v3",
        4 => "Chaos",
        6 => "Private Match",
        7 => "Season",
        12 => "Solo Standard",
        15 | 30 => "Snow Day",
        16 | 27 => "Hoops",
        17 | 28 => "Rumble",
        18 | 29 => "Dropshot",
        22 => "Tournament",
        63 => "Heatseeker",
        _ => "Unknown Mode",
    }
}

fn unix_date(timestamp: i64) -> String {
    // Convert Unix days to a proleptic Gregorian date without pulling a date
    // library into the desktop binary.
    let days = timestamp.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn validate_replay_url(value: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| "PsyNet returned an invalid replay URL".to_string())?;
    let trusted_host = url
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("api.rlpp.psynet.gg"));
    if !trusted_host || !matches!(url.scheme(), "http" | "https") {
        return Err("PsyNet returned an invalid replay URL".to_string());
    }
    Ok(url)
}

fn visibility(value: &str) -> &'static str {
    match value {
        "public" => "public",
        "unlisted" => "unlisted",
        _ => "private",
    }
}

fn normalize_guid(value: &str) -> String {
    value.trim().to_ascii_uppercase()
}

#[cfg(test)]
mod replay_lifecycle_tests {
    use super::*;
    #[test]
    fn disabling_capture_cancels_pending_replay_jobs() {
        // new only reads history; this test never writes files or uses the network.
        let mut uploader =
            AutoUploadReplays::new(&std::env::temp_dir().join("hebnix-replay-dependency-test"));
        uploader.sync(true, false, false);
        assert!(!uploader.running());
        assert_eq!(uploader.status, "Waiting for Rocket League");
        uploader.sync(true, true, false);
        assert!(!uploader.running());
        uploader.sync(true, true, true);
        assert!(uploader.running());
        assert!(uploader.needs_match_identity(Some("test-guid")));
        uploader.track_match("test-guid", Some("test-player".into()));
        assert!(!uploader.needs_match_identity(Some("test-guid")));
        uploader.pending.insert("test-guid".into());
        let generation = uploader.generation.load(Ordering::Acquire);
        uploader.sync(false, true, true);
        assert!(!uploader.running());
        assert!(uploader.current_match.is_none() && uploader.pending.is_empty());
        assert!(uploader.generation.load(Ordering::Acquire) > generation);
        assert_eq!(uploader.status, "Disabled");
        assert!(!uploader.needs_match_identity(Some("test-guid")));
        uploader.sync(true, true, true);
        assert!(uploader.needs_match_identity(Some("test-guid")));
        uploader.submitted.insert(normalize_guid("test-guid"));
        assert!(!uploader.needs_match_identity(Some("test-guid")));
    }
}

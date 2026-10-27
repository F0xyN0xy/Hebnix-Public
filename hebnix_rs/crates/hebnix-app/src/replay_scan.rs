use hebnix_sdk::stats::StatsEvent;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Marker {
    pub frame: i64,
    pub event: String,
    pub player: String,
    pub team: i64,
}

#[derive(Serialize, Deserialize)]
pub struct CapturedEvent {
    frame: i64,
    event: String,
    data: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
pub struct EventCache {
    version: u32,
    replay_sha256: String,
    frames: i64,
    fps: f64,
    pub markers: Vec<Marker>,
    events: Vec<CapturedEvent>,
}

pub struct CacheKey {
    pub path: PathBuf,
    digest: String,
    frames: i64,
    fps: f64,
}
impl CacheKey {
    pub fn new(replay: &Path, directory: &Path, frames: i64, fps: f64) -> Result<Self, String> {
        let mut file = std::fs::File::open(replay).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 65536];
        loop {
            let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        let digest = format!("{:x}", hash.finalize());
        Ok(Self {
            path: directory.join(format!("{digest}.json")),
            digest,
            frames,
            fps,
        })
    }
    pub fn load(&self) -> Option<EventCache> {
        let file = std::fs::File::open(&self.path).ok()?;
        if file.metadata().ok()?.len() > 64 * 1024 * 1024 {
            return None;
        }
        let cache: EventCache = serde_json::from_reader(file).ok()?;
        (cache.version == 1
            && cache.replay_sha256 == self.digest
            && cache.frames == self.frames
            && cache.fps == self.fps
            && cache
                .markers
                .iter()
                .all(|m| (1..=self.frames).contains(&m.frame)))
        .then_some(cache)
    }
    pub fn save(&self, markers: Vec<Marker>, events: Vec<CapturedEvent>) -> Result<(), String> {
        std::fs::create_dir_all(self.path.parent().ok_or("Missing cache directory")?)
            .map_err(|e| e.to_string())?;
        let cache = EventCache {
            version: 1,
            replay_sha256: self.digest.clone(),
            frames: self.frames,
            fps: self.fps,
            markers,
            events,
        };
        let temporary = self.path.with_extension("json.tmp");
        let result = (|| {
            let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
            serde_json::to_writer(&mut file, &cache).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            std::fs::rename(&temporary, &self.path).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    }
}

pub struct Scan {
    pub key: CacheKey,
    pub armed: bool,
    pub at_start: bool,
    pub running: bool,
    pub frame: i64,
    pub last_progress: Instant,
    pub started: Instant,
    pub reached_end: Option<Instant>,
    pub interrupted: bool,
    pub resume_attempts: u8,
    pub resume_after: Instant,
    pub markers: Vec<Marker>,
    pub events: Vec<CapturedEvent>,
    pending: Vec<(String, serde_json::Value)>,
    counters: HashMap<(String, &'static str), i64>,
}
impl Scan {
    pub fn new(key: CacheKey, goals: Vec<Marker>) -> Self {
        Self {
            key,
            armed: false,
            at_start: false,
            running: false,
            frame: 1,
            last_progress: Instant::now(),
            started: Instant::now(),
            reached_end: None,
            markers: goals,
            events: vec![],
            pending: vec![],
            counters: HashMap::new(),
            interrupted: false,
            resume_attempts: 0,
            resume_after: Instant::now(),
        }
    }
    pub fn observe(&mut self, event: &StatsEvent) {
        if let Some(state) = event.update_state() {
            let frame = state.game.frame.saturating_sub(1).clamp(1, self.key.frames);
            if !self.armed || self.interrupted {
                return;
            }
            if !self.running {
                if frame > 2 {
                    return;
                }
                self.at_start = true;
                self.counters.clear();
            } else if frame < self.frame {
                // Ignore late seek packets. The scan never intentionally rewinds.
                if self.frame - frame > (self.key.fps * 2.0) as i64 {
                    self.interrupted = true;
                }
                return;
            }
            if self.running
                && frame - self.frame
                    > (self.key.fps
                        * (64.0 * self.last_progress.elapsed().as_secs_f64().max(0.1) + 1.0))
                        as i64
            {
                self.interrupted = true;
                return;
            }
            if frame > self.frame {
                self.last_progress = Instant::now();
            }
            self.frame = frame;
            for player in &state.players {
                let identity = if player.primary_id.is_empty() {
                    format!("{}:{}", player.name, player.team_num)
                } else {
                    player.primary_id.clone()
                };
                for (name, value) in [
                    ("Save", player.saves),
                    ("Shot", player.shots),
                    ("Assist", player.assists),
                    ("Demolition", player.demos),
                ] {
                    let previous = self
                        .counters
                        .entry((identity.clone(), name))
                        .or_insert(value);
                    if self.running && value > *previous {
                        for _ in 0..(value - *previous).min(100) {
                            self.markers.push(Marker {
                                frame,
                                event: name.into(),
                                player: player.name.clone(),
                                team: player.team_num,
                            });
                        }
                    }
                    *previous = (*previous).max(value);
                }
            }
            self.flush_events(frame);
            if self.running && frame >= self.key.frames - 1 && self.reached_end.is_none() {
                self.reached_end = Some(Instant::now());
            }
        } else if self.running
            && !matches!(
                event.event_type.as_str(),
                "ReplayCreated"
                    | "MatchCreated"
                    | "MatchDestroyed"
                    | "MatchPaused"
                    | "MatchUnpaused"
            )
        {
            if self.pending.len() < 4096 && self.events.len() < 200_000 {
                self.pending
                    .push((event.event_type.clone(), event.raw_data.clone()));
            }
        }
    }
    pub fn flush_events(&mut self, frame: i64) {
        for (event, data) in self.pending.drain(..) {
            // The API has no Frame field on stat-feed messages. Use the following state
            // packet to place them; retain raw payloads in the JSON for future consumers.
            if event == "StatfeedEvent" {
                let name = data["EventName"].as_str().unwrap_or("");
                let known_counter = [
                    "save",
                    "epicsave",
                    "shot",
                    "shotongoal",
                    "assist",
                    "demolish",
                    "demolition",
                    "goal",
                ]
                .iter()
                .any(|n| name.eq_ignore_ascii_case(n));
                if !known_counter && !name.is_empty() {
                    self.markers.push(Marker {
                        frame,
                        event: data["Type"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .unwrap_or(name)
                            .into(),
                        player: data["MainTarget"]["Name"].as_str().unwrap_or("").into(),
                        team: data["MainTarget"]["TeamNum"].as_i64().unwrap_or(255),
                    });
                }
            }
            self.events.push(CapturedEvent { frame, event, data });
        }
    }
    pub fn finished(&self) -> bool {
        self.reached_end
            .is_some_and(|at| at.elapsed() >= Duration::from_millis(350))
    }
}

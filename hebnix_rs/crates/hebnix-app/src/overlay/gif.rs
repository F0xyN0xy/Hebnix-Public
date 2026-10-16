//! gif decoding shared by the native overlay backends. Frames come back as
//! straight-alpha RGBA, each backend converts them to its own bitmap format.

use image::{AnimationDecoder, RgbaImage};

/// most frames / decoded bytes kept for one gif, and the longest side a
/// frame is shrunk to (avatars are tiny, a huge gif would eat memory)
const MAX_FRAMES: usize = 200;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_SIDE: u32 = 256;
/// browsers treat 0/10ms delays as 100ms, do the same
const MIN_DELAY_MS: u32 = 20;
const DEFAULT_DELAY_MS: u32 = 100;

pub struct Frames {
    pub images: Vec<RgbaImage>,
    /// per-frame display time
    pub delays_ms: Vec<u32>,
    pub total_ms: u64,
}

impl Frames {
    /// index of the frame showing `elapsed_ms` into the loop
    pub fn index_at(&self, elapsed_ms: u64) -> usize {
        index_at(&self.delays_ms, self.total_ms, elapsed_ms)
    }
}

pub fn index_at(delays_ms: &[u32], total_ms: u64, elapsed_ms: u64) -> usize {
    let mut t = elapsed_ms % total_ms.max(1);
    for (i, &delay) in delays_ms.iter().enumerate() {
        if t < delay as u64 {
            return i;
        }
        t -= delay as u64;
    }
    delays_ms.len().saturating_sub(1)
}

/// shared clock so every animated image on screen loops from process start
pub fn clock_ms() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
}

/// decode any supported image file. A gif with more than one frame gives all
/// its frames, everything else gives a single frame with no delay.
pub fn load(path: &str) -> Option<Frames> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(b"GIF8") {
        if let Some(frames) = decode_gif(&bytes) {
            return Some(frames);
        }
    }
    let img = image::load_from_memory(&bytes).ok()?.into_rgba8();
    Some(Frames {
        images: vec![img],
        delays_ms: vec![DEFAULT_DELAY_MS],
        total_ms: DEFAULT_DELAY_MS as u64,
    })
}

pub fn decode_gif(bytes: &[u8]) -> Option<Frames> {
    let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).ok()?;
    let mut images = Vec::new();
    let mut delays_ms = Vec::new();
    let mut used = 0usize;
    for frame in decoder.into_frames() {
        let Ok(frame) = frame else { break };
        let (num, den) = frame.delay().numer_denom_ms();
        let mut delay = num.checked_div(den).unwrap_or(0);
        if delay < MIN_DELAY_MS {
            delay = DEFAULT_DELAY_MS;
        }
        let mut buf = frame.into_buffer();
        let (w, h) = buf.dimensions();
        if w.max(h) > MAX_SIDE {
            let scale = MAX_SIDE as f32 / w.max(h) as f32;
            let nw = ((w as f32 * scale).round() as u32).max(1);
            let nh = ((h as f32 * scale).round() as u32).max(1);
            buf = image::imageops::resize(&buf, nw, nh, image::imageops::FilterType::Triangle);
        }
        used += buf.width() as usize * buf.height() as usize * 4;
        if images.len() >= MAX_FRAMES || used > MAX_BYTES {
            break;
        }
        images.push(buf);
        delays_ms.push(delay);
    }
    if images.is_empty() {
        return None;
    }
    let total_ms = delays_ms.iter().map(|&d| d as u64).sum();
    Some(Frames {
        images,
        delays_ms,
        total_ms,
    })
}

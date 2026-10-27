use rodio::{Decoder, OutputStream, Sink, Source, buffer::SamplesBuffer};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Cursor, Read},
    path::Path,
    sync::Arc,
    time::Instant,
};

#[derive(Clone)]
pub(super) struct Audio {
    pub samples: Arc<Vec<i16>>,
    pub channels: u16,
    pub rate: u32,
    pub peaks: Vec<f32>,
}
impl Audio {
    pub fn decode(path: &Path) -> Result<Self, String> {
        let file = fs::File::open(path).map_err(|e| e.to_string())?;
        let decoder = Decoder::new(std::io::BufReader::new(file))
            .map_err(|e| format!("Could not decode audio: {e}"))?;
        let channels = decoder.channels();
        let rate = decoder.sample_rate();
        if !(1..=2).contains(&channels) || rate == 0 || rate > 192_000 {
            return Err("Import mono or stereo audio at up to 192 kHz".into());
        }
        let samples: Vec<i16> = decoder.convert_samples().take(60_000_001).collect();
        if samples.is_empty() || samples.len() > 60_000_000 {
            return Err("Audio is empty or too long (maximum 60 million samples)".into());
        }
        Ok(Self::new(samples, channels, rate))
    }
    pub(super) fn new(samples: Vec<i16>, channels: u16, rate: u32) -> Self {
        let step = samples.len().div_ceil(1200).max(1);
        let peaks = samples
            .chunks(step)
            .map(|s| {
                s.iter()
                    .map(|v| (*v as f32).abs() / 32768.0)
                    .fold(0.0, f32::max)
            })
            .collect();
        Self {
            samples: Arc::new(samples),
            channels,
            rate,
            peaks,
        }
    }
    pub fn duration(&self) -> f64 {
        self.samples.len() as f64 / self.channels as f64 / self.rate as f64
    }
    pub fn max_start(&self, length: f64) -> Option<f64> {
        if !length.is_finite() || length <= 0.0 {
            return None;
        }
        let frames = (length * self.rate as f64).round() as usize;
        if frames == 0 {
            return None;
        }
        (self.samples.len() / self.channels as usize)
            .checked_sub(frames)
            .map(|remaining| remaining as f64 / self.rate as f64)
    }
    pub fn section_bounds(
        &self,
        start: f64,
        length: f64,
    ) -> Result<std::ops::Range<usize>, String> {
        if !start.is_finite() || !length.is_finite() || start < 0.0 || length <= 0.0 {
            return Err("Invalid audio selection".into());
        }
        let max_start = self
            .max_start(length)
            .ok_or("The imported song is shorter than the required selection")?;
        if start > max_start + 0.5 / self.rate as f64 + 1e-9 {
            return Err("The selected interval extends beyond the imported audio".into());
        }
        let first =
            (start.min(max_start) * self.rate as f64).round() as usize * self.channels as usize;
        let count = (length * self.rate as f64).round() as usize * self.channels as usize;
        let end = first
            .checked_add(count)
            .ok_or("Audio selection size overflow")?;
        if end > self.samples.len() {
            return Err("The imported song is shorter than the required selection".into());
        }
        Ok(first..end)
    }
    pub fn section(&self, start: f64, length: f64) -> Result<Self, String> {
        let range = self.section_bounds(start, length)?;
        Ok(Self::new(
            self.samples[range].to_vec(),
            self.channels,
            self.rate,
        ))
    }
    pub fn wav(&self) -> Vec<u8> {
        let size = self.samples.len() as u32 * 2;
        let mut b = Vec::with_capacity(size as usize + 44);
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(size + 36).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&self.channels.to_le_bytes());
        b.extend_from_slice(&self.rate.to_le_bytes());
        b.extend_from_slice(&(self.rate * self.channels as u32 * 2).to_le_bytes());
        b.extend_from_slice(&(self.channels * 2).to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&size.to_le_bytes());
        for s in self.samples.iter() {
            b.extend_from_slice(&s.to_le_bytes());
        }
        b
    }
    pub fn wem(&self) -> Vec<u8> {
        // Wwise's AkWaveFormatExtensible PCM: 24-byte fmt with the
        // serialized AkChannelConfig. A normal 16-byte WAV fmt omits it.
        let mut wav = self.wav();
        wav[20..22].copy_from_slice(&0xfffeu16.to_le_bytes());
        wav[16..20].copy_from_slice(&24u32.to_le_bytes());
        let mask = if self.channels == 1 { 4u32 } else { 3u32 };
        let config = (mask << 12) | 0x100 | self.channels as u32;
        let mut extra = Vec::new();
        extra.extend_from_slice(&6u16.to_le_bytes());
        extra.extend_from_slice(&0u16.to_le_bytes());
        extra.extend_from_slice(&config.to_le_bytes());
        wav.splice(36..36, extra);
        // Align the PCM payload itself as well as its media entry.
        wav.splice(44..44, [b'J', b'U', b'N', b'K', 4, 0, 0, 0, 0, 0, 0, 0]);
        let size = (wav.len() - 8) as u32;
        wav[4..8].copy_from_slice(&size.to_le_bytes());
        wav
    }
    pub fn from_pcm_wem(bytes: &[u8]) -> Result<Self, String> {
        if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
            return Err("Invalid Wwise PCM RIFF".into());
        }
        let mut at = 12;
        let mut format = None;
        let mut pcm = None;
        while at + 8 <= bytes.len() {
            let n = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            let payload = bytes
                .get(at + 8..at + 8 + n)
                .ok_or("Truncated Wwise PCM chunk")?;
            match &bytes[at..at + 4] {
                b"fmt " => format = Some(payload),
                b"data" => pcm = Some(payload),
                _ => {}
            }
            at += 8 + n + (n & 1);
        }
        let format = format.ok_or("Missing Wwise PCM format")?;
        if format.len() != 24
            || u16::from_le_bytes(format[..2].try_into().unwrap()) != 0xfffe
            || format[14..16] != 16u16.to_le_bytes()
        {
            return Err("Invalid Wwise PCM format".into());
        }
        let channels = u16::from_le_bytes(format[2..4].try_into().unwrap());
        let rate = u32::from_le_bytes(format[4..8].try_into().unwrap());
        if !(1..=2).contains(&channels) || rate == 0 {
            return Err("Invalid Wwise PCM channel count or rate".into());
        }
        let pcm = pcm.ok_or("Missing Wwise PCM data")?;
        if pcm.len() % (channels as usize * 2) != 0 {
            return Err("Incomplete Wwise PCM frame".into());
        }
        Ok(Self::new(
            pcm.chunks_exact(2)
                .map(|s| i16::from_le_bytes(s.try_into().unwrap()))
                .collect(),
            channels,
            rate,
        ))
    }
}

pub(super) struct Player {
    sink: Sink,
    _stream: OutputStream,
    started: Instant,
    start: f64,
    end: f64,
    paused_at: Option<f64>,
}
impl Player {
    pub fn play(audio: &Audio, start: f64, end: f64) -> Result<Self, String> {
        let clip = audio.section(start, end - start)?;
        let (stream, handle) =
            OutputStream::try_default().map_err(|e| format!("Audio output: {e}"))?;
        let sink = Sink::try_new(&handle).map_err(|e| e.to_string())?;
        sink.append(SamplesBuffer::new(
            clip.channels,
            clip.rate,
            clip.samples.as_ref().clone(),
        ));
        Ok(Self {
            sink,
            _stream: stream,
            started: Instant::now(),
            start,
            end,
            paused_at: None,
        })
    }
    pub fn position(&self) -> f64 {
        self.paused_at
            .unwrap_or_else(|| (self.start + self.started.elapsed().as_secs_f64()).min(self.end))
    }
    pub fn toggle(&mut self) {
        if let Some(at) = self.paused_at.take() {
            self.start = at;
            self.started = Instant::now();
            self.sink.play();
        } else {
            self.paused_at = Some(self.position());
            self.sink.pause();
        }
    }
    pub fn paused(&self) -> bool {
        self.paused_at.is_some()
    }
    pub fn finished(&self) -> bool {
        self.sink.empty()
    }
}

// Official pinned decoder, installed on demand on a worker thread. Keep the
// archive checksum and version together; never execute an unverified download.
pub(super) fn original_audio(base: &Path, path: &Path) -> Result<Audio, String> {
    let tool_dir = base.join("tools/vgmstream-r2168");
    let exe = tool_dir.join("vgmstream-cli.exe");
    if !exe.is_file() {
        let response = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(90)).build()
            .get("https://github.com/vgmstream/vgmstream/releases/download/r2168/vgmstream-win64.zip")
            .call().map_err(|e| format!("Could not download anthem audio decoder: {e}"))?;
        let mut zip = Vec::new();
        response
            .into_reader()
            .take(64 * 1024 * 1024)
            .read_to_end(&mut zip)
            .map_err(|e| e.to_string())?;
        if hex::encode(Sha256::digest(&zip))
            != "692d45070852a35c1fa42726aa3c9de528e44eec925d37d99b1074256b99a229"
        {
            return Err("Anthem decoder checksum mismatch".into());
        }
        fs::create_dir_all(&tool_dir).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(Cursor::new(zip)).map_err(|e| e.to_string())?;
        // Extract the executable last, so an interrupted install retries cleanly.
        for pass in [false, true] {
            for i in 0..archive.len() {
                let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
                let Some(name) = entry.enclosed_name() else {
                    return Err("Unsafe decoder archive path".into());
                };
                if entry.is_dir()
                    || name.components().count() != 1
                    || (name == Path::new("vgmstream-cli.exe")) != pass
                {
                    continue;
                }
                let mut out = fs::File::create(tool_dir.join(name)).map_err(|e| e.to_string())?;
                std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
            }
        }
    }
    let preview_dir = base.join("anthems/preview");
    fs::create_dir_all(&preview_dir).map_err(|e| e.to_string())?;
    let output = preview_dir.join(format!("decode-{}.wav", rand::random::<u64>()));
    let mut command = std::process::Command::new(&exe);
    command.args(["-i", "-o"]).arg(&output).arg(path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let result = (|| {
        let status = command
            .output()
            .map_err(|e| format!("Could not start anthem decoder: {e}"))?;
        if !status.status.success() {
            return Err(format!(
                "Anthem decoding failed: {}",
                String::from_utf8_lossy(&status.stderr)
            ));
        }
        Audio::decode(&output)
    })();
    let _ = fs::remove_file(output);
    result
}

//! The inspected Rocket League Wwise 150 layout. See wwiser's wparser.py
//! (CAkMusicTrack__SetInitialValues / CAkBankMgr__LoadSource).
//! UPKs contain event references; media and clip timings belong to the BNK.
use std::collections::{BTreeSet, HashMap};

pub(super) fn u32_at(b: &[u8], p: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4)
            .ok_or("Truncated sound bank")?
            .try_into()
            .unwrap(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_events_keep_other_tracks_and_original_media_intact() {
        let original = super::super::audio::Audio::new(vec![1000; 1000], 1, 1000).wav();
        let custom = super::super::audio::Audio::new(vec![-2000; 1000], 1, 1000).wav();
        let mut objects = Vec::new();
        for (event, id) in [
            ("Play_First_Goal", 0x12345670u32),
            ("Play_Second_Goal", 0x23456780u32),
        ] {
            let mut action = vec![3, 4];
            action.extend_from_slice(&(id + 1).to_le_bytes());
            let mut event_data = vec![1];
            event_data.extend_from_slice(&id.to_le_bytes());
            let mut segment = 1u32.to_le_bytes().to_vec();
            segment.extend_from_slice(&(id + 2).to_le_bytes());
            let mut track = vec![0; 67];
            track[1..5].copy_from_slice(&1u32.to_le_bytes());
            track[5..9].copy_from_slice(&0x40001u32.to_le_bytes());
            track[10..14].copy_from_slice(&100u32.to_le_bytes());
            track[19..23].copy_from_slice(&1u32.to_le_bytes());
            track[27..31].copy_from_slice(&100u32.to_le_bytes());
            track[59..67].copy_from_slice(&1000.0f64.to_le_bytes());
            objects.extend([
                (
                    event_id(event),
                    Object {
                        kind: 4,
                        data: event_data,
                    },
                ),
                (
                    id,
                    Object {
                        kind: 3,
                        data: action,
                    },
                ),
                (
                    id + 1,
                    Object {
                        kind: 10,
                        data: segment,
                    },
                ),
                (
                    id + 2,
                    Object {
                        kind: 11,
                        data: track,
                    },
                ),
            ]);
        }
        let mut didx = Vec::new();
        for n in [100, 0, original.len() as u32] {
            didx.extend_from_slice(&n.to_le_bytes());
        }
        let index = objects
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (*id, i))
            .collect();
        let mut bank = Bank {
            chunks: vec![
                (*b"BKHD", {
                    let mut header = vec![0; 44];
                    header[..4].copy_from_slice(&150u32.to_le_bytes());
                    header[12..16].copy_from_slice(&16u32.to_le_bytes());
                    header
                }),
                (*b"DIDX", didx),
                (*b"DATA", original.clone()),
                (*b"HIRC", vec![]),
            ],
            objects,
            index,
        };
        let untouched = bank.object(0x23456782).unwrap().data.clone();
        let first = bank.clips("Play_First_Goal").unwrap().remove(0);
        bank.replace_clip(&first, 101, &custom).unwrap();
        Bank::validate_media_alignment(&bank.encode()).unwrap();
        let parsed = Bank::parse(&bank.encode()).unwrap();
        assert_eq!(parsed.object(0x23456782).unwrap().data, untouched);
        assert_eq!(parsed.embedded(100).unwrap(), original);
        assert_eq!(parsed.embedded(101).unwrap(), custom);
        let second = bank.clips("Play_Second_Goal").unwrap().remove(0);
        bank.replace_clip(&second, 99, &custom).unwrap();
        Bank::validate_media_alignment(&bank.encode()).unwrap();
        let parsed = Bank::parse(&bank.encode()).unwrap();
        assert_eq!(parsed.clips("Play_First_Goal").unwrap()[0].source, 101);
        assert_eq!(parsed.clips("Play_Second_Goal").unwrap()[0].source, 99);
        assert_eq!(parsed.embedded(100).unwrap(), original);
    }
}
fn f64_at(b: &[u8], p: usize) -> Result<f64, String> {
    let v = f64::from_le_bytes(
        b.get(p..p + 8)
            .ok_or("Truncated music clip")?
            .try_into()
            .unwrap(),
    );
    if !v.is_finite() {
        return Err("Invalid music clip timing".into());
    }
    Ok(v)
}
pub(super) fn event_id(s: &str) -> u32 {
    s.bytes().fold(2166136261u32, |h, c| {
        h.wrapping_mul(16777619) ^ c.to_ascii_lowercase() as u32
    })
}
#[derive(Clone, Debug)]
pub(super) struct Clip {
    pub track: u32,
    pub source: u32,
    pub start: f64,
    pub duration: f64,
    pub play_at: f64,
    pub playlist_offset: usize,
}
#[derive(Clone)]
struct Object {
    kind: u8,
    data: Vec<u8>,
}
pub(super) struct Bank {
    chunks: Vec<([u8; 4], Vec<u8>)>,
    objects: Vec<(u32, Object)>,
    index: HashMap<u32, usize>,
}
impl Bank {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let mut chunks = Vec::new();
        let mut p = 0usize;
        while p < bytes.len() {
            let tag = bytes
                .get(p..p + 4)
                .ok_or("Truncated bank chunk")?
                .try_into()
                .unwrap();
            let n = u32_at(bytes, p + 4)? as usize;
            let end = p
                .checked_add(8)
                .and_then(|p| p.checked_add(n))
                .ok_or("Bank size overflow")?;
            chunks.push((
                tag,
                bytes
                    .get(p + 8..end)
                    .ok_or("Truncated bank payload")?
                    .to_vec(),
            ));
            p = end;
        }
        let header = &chunks
            .iter()
            .find(|c| &c.0 == b"BKHD")
            .ok_or("Missing BKHD")?
            .1;
        if u32_at(header, 0)? != 150 {
            return Err("This anthem uses an unsupported Wwise bank version (expected 150)".into());
        }
        let hirc = &chunks
            .iter()
            .find(|c| &c.0 == b"HIRC")
            .ok_or("Missing HIRC")?
            .1;
        let count = u32_at(hirc, 0)? as usize;
        if count > 100_000 {
            return Err("Invalid bank object count".into());
        }
        let mut objects = Vec::new();
        let mut index = HashMap::new();
        p = 4;
        for _ in 0..count {
            let kind = *hirc.get(p).ok_or("Truncated bank object")?;
            let n = u32_at(hirc, p + 1)? as usize;
            if n < 4 {
                return Err("Invalid bank object size".into());
            }
            let id = u32_at(hirc, p + 5)?;
            let data = hirc
                .get(p + 9..p + 5 + n)
                .ok_or("Truncated bank object data")?
                .to_vec();
            if index.insert(id, objects.len()).is_some() {
                return Err("Duplicate bank object ID".into());
            }
            objects.push((id, Object { kind, data }));
            p += 5 + n;
        }
        if p != hirc.len() {
            return Err("Unexpected HIRC trailing data".into());
        }
        Ok(Self {
            chunks,
            objects,
            index,
        })
    }
    fn object(&self, id: u32) -> Result<&Object, String> {
        Ok(&self.objects[*self
            .index
            .get(&id)
            .ok_or_else(|| format!("Missing bank object {id}"))?]
        .1)
    }
    fn children(&self, id: u32) -> Result<Vec<u32>, String> {
        let o = self.object(id)?;
        match o.kind {
            4 => {
                // Event: AkUInt32 variable-length action count (one byte for these banks).
                let count = *o.data.first().ok_or("Empty event")? as usize;
                if count >= 128 || o.data.len() != 1 + count * 4 {
                    return Err("Unsupported anthem event action list".into());
                }
                (0..count).map(|i| u32_at(&o.data, 1 + i * 4)).collect()
            }
            3 => {
                if o.data.get(1) != Some(&4) {
                    return Err("Anthem start event is not a Play action".into());
                }
                Ok(vec![u32_at(&o.data, 2)?])
            }
            10 | 13 => {
                // Music nodes serialize a counted child array among their parameters.
                // Accept a single distinct array of correctly typed local objects only.
                let mut candidates = BTreeSet::new();
                for at in 0..o.data.len().saturating_sub(7) {
                    let n = u32_at(&o.data, at)? as usize;
                    if n == 0 || n > 128 || at + 4 + n * 4 > o.data.len() {
                        continue;
                    }
                    let ids: Vec<_> = (0..n)
                        .map(|i| u32_at(&o.data, at + 4 + i * 4).unwrap())
                        .collect();
                    if ids.iter().all(|child| {
                        *child != id
                            && self.object(*child).is_ok_and(|v| {
                                if o.kind == 10 {
                                    v.kind == 11
                                } else {
                                    matches!(v.kind, 10 | 13)
                                }
                            })
                    }) {
                        candidates.insert(ids);
                    }
                }
                // Single-child suffixes of a longer array are not separate arrays.
                let longest = candidates
                    .iter()
                    .max_by_key(|v| v.len())
                    .ok_or("No supported music children")?
                    .clone();
                if candidates.iter().any(|v| !longest.ends_with(v)) {
                    return Err("Ambiguous anthem music graph".into());
                }
                Ok(longest)
            }
            11 => Ok(Vec::new()),
            kind => Err(format!("Unsupported anthem node type {kind}")),
        }
    }
    pub fn clips(&self, event: &str) -> Result<Vec<Clip>, String> {
        let mut pending = vec![event_id(event)];
        let mut visited = BTreeSet::new();
        let mut clips = Vec::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            let o = self.object(id)?;
            if o.kind != 11 {
                pending.extend(self.children(id)?);
                continue;
            }
            let b = &o.data;
            if u32_at(b, 1)? != 1 || !matches!(u32_at(b, 5)?, 0x00040001 | 0x00010001) {
                return Err(format!(
                    "Anthem track {id} has {} sources (first codec {:08x})",
                    u32_at(b, 1)?,
                    u32_at(b, 5)?
                ));
            }
            let source = u32_at(b, 10)?;
            let count = u32_at(b, 19)? as usize;
            if count > 1000 {
                return Err("Invalid anthem clip count".into());
            }
            for i in 0..count {
                let at = 23 + i * 44;
                let clip_source = u32_at(b, at + 4)?;
                if clip_source == 0 {
                    continue;
                } // Event-only/silent playlist clip.
                if clip_source != source {
                    return Err("Anthem playlist references another source".into());
                }
                let start = f64_at(b, at + 20)? / 1000.0;
                let duration = (f64_at(b, at + 36)? + f64_at(b, at + 28)?) / 1000.0 - start;
                let play_at = f64_at(b, at + 12)? / 1000.0 + start;
                if start < 0.0 || !(0.001..600.0).contains(&duration) || play_at.abs() > 0.1 {
                    return Err(format!(
                        "Anthem track {id} timing: start={start:.9}s duration={duration:.9}s play_at={play_at:.9}s"
                    ));
                }
                clips.push(Clip {
                    track: id,
                    source,
                    start,
                    duration,
                    play_at,
                    playlist_offset: at,
                });
            }
        }
        if clips.is_empty() {
            return Err("This anthem event has no playable audio".into());
        }
        clips.sort_by_key(|c| c.track);
        Ok(clips)
    }
    pub fn embedded(&self, source: u32) -> Result<Vec<u8>, String> {
        let idx = &self
            .chunks
            .iter()
            .find(|c| &c.0 == b"DIDX")
            .ok_or("No embedded audio")?
            .1;
        let data = &self
            .chunks
            .iter()
            .find(|c| &c.0 == b"DATA")
            .ok_or("No embedded audio data")?
            .1;
        for row in idx.chunks_exact(12) {
            if u32_at(row, 0)? == source {
                let off = u32_at(row, 4)? as usize;
                let n = u32_at(row, 8)? as usize;
                return Ok(data
                    .get(off..off + n)
                    .ok_or("Invalid embedded audio range")?
                    .to_vec());
            }
        }
        Err("Audio source not present in bank".into())
    }
    pub fn replace_clip(&mut self, clip: &Clip, media_id: u32, wav: &[u8]) -> Result<(), String> {
        let o = &mut self.objects[*self
            .index
            .get(&clip.track)
            .ok_or("Missing replacement track")?]
        .1;
        // A dedicated in-memory PCM source leaves the original streamed recording,
        // prefetch data, other events, and all unrelated bank objects intact.
        o.data[5..9].copy_from_slice(&0x00010001u32.to_le_bytes());
        o.data[9] = 0; // Data / in-memory media.
        o.data[10..14].copy_from_slice(&media_id.to_le_bytes());
        o.data[14..18].copy_from_slice(&(wav.len() as u32).to_le_bytes());
        o.data[18] &= !2; // No prefetch.
        let at = clip.playlist_offset;
        o.data[at + 4..at + 8].copy_from_slice(&media_id.to_le_bytes());
        for (off, value) in [
            (12, clip.play_at * 1000.0),
            (20, 0.0),
            (28, 0.0),
            (36, clip.duration * 1000.0),
        ] {
            o.data[at + off..at + off + 8].copy_from_slice(&value.to_le_bytes());
        }
        for tag in [*b"DIDX", *b"DATA"] {
            if !self.chunks.iter().any(|c| c.0 == tag) {
                let before_hirc = self
                    .chunks
                    .iter()
                    .position(|c| &c.0 == b"HIRC")
                    .unwrap_or(self.chunks.len());
                self.chunks.insert(before_hirc, (tag, Vec::new()));
            }
        }
        let alignment = (u32_at(&self.chunks.iter().find(|c| &c.0 == b"BKHD").unwrap().1, 12)
            .unwrap_or(16)
            & 0xffff)
            .max(1) as usize;
        let data = &mut self.chunks.iter_mut().find(|c| &c.0 == b"DATA").unwrap().1;
        while data.len() % alignment != 0 {
            data.push(0);
        }
        let off = data.len() as u32;
        data.extend_from_slice(wav);
        let idx = &mut self.chunks.iter_mut().find(|c| &c.0 == b"DIDX").unwrap().1;
        for value in [media_id, off, wav.len() as u32] {
            idx.extend_from_slice(&value.to_le_bytes());
        }
        Ok(())
    }
    pub fn media_ids(&self) -> BTreeSet<u32> {
        let mut ids: BTreeSet<_> = self
            .chunks
            .iter()
            .find(|c| &c.0 == b"DIDX")
            .map(|c| {
                c.1.chunks_exact(12)
                    .filter_map(|v| u32_at(v, 0).ok())
                    .collect()
            })
            .unwrap_or_default();
        for (_, o) in &self.objects {
            if o.kind == 11
                && let Ok(id) = u32_at(&o.data, 10)
            {
                ids.insert(id);
            }
        }
        ids
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut hirc = (self.objects.len() as u32).to_le_bytes().to_vec();
        for (id, o) in &self.objects {
            hirc.push(o.kind);
            hirc.extend_from_slice(&((o.data.len() + 4) as u32).to_le_bytes());
            hirc.extend_from_slice(&id.to_le_bytes());
            hirc.extend_from_slice(&o.data);
        }
        let mut chunks = self.chunks.clone();
        if let Some((_, index)) = chunks.iter_mut().find(|c| &c.0 == b"DIDX") {
            // Wwise looks media up by ID. Keep its lookup table sorted even
            // though media payloads retain their original append order.
            let mut rows: Vec<_> = index.chunks_exact(12).map(|r| r.to_vec()).collect();
            rows.sort_by_key(|r| u32_at(r, 0).unwrap());
            *index = rows.into_iter().flatten().collect();
        }
        if let (Some(header), Some(data)) = (
            chunks.iter().position(|c| &c.0 == b"BKHD"),
            chunks.iter().position(|c| &c.0 == b"DATA"),
        ) {
            if header < data && chunks[header].1.len() >= 40 {
                let alignment =
                    (u32_at(&chunks[header].1, 12).unwrap_or(16) & 0xffff).max(1) as usize;
                if alignment.is_power_of_two() && alignment <= 65536 {
                    // BKHD's trailing bytes are padding, not part of its hash or
                    // fields. Growing DIDX changes the absolute DATA address.
                    chunks[header].1.truncate(40);
                    let data_start: usize = chunks[..data]
                        .iter()
                        .map(|(_, v)| v.len() + 8)
                        .sum::<usize>()
                        + 8;
                    let padding = (alignment - data_start % alignment) % alignment;
                    chunks[header].1.resize(40 + padding, 0);
                }
            }
        }
        let mut out = Vec::new();
        for (tag, bytes) in &chunks {
            let data = if tag == b"HIRC" { &hirc } else { bytes };
            out.extend_from_slice(tag);
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(data);
        }
        out
    }
    pub fn validate_media_alignment(bytes: &[u8]) -> Result<(), String> {
        let bank = Self::parse(bytes)?;
        let header = &bank
            .chunks
            .iter()
            .find(|c| &c.0 == b"BKHD")
            .ok_or("Missing bank header")?
            .1;
        let alignment = (u32_at(header, 12)? & 0xffff).max(1) as usize;
        if !alignment.is_power_of_two() || alignment > 65536 {
            return Err("Invalid Wwise media alignment".into());
        }
        let mut start = 0;
        let mut data_start = None;
        for (tag, data) in &bank.chunks {
            if tag == b"DATA" {
                data_start = Some(start + 8);
                break;
            }
            start += 8 + data.len();
        }
        let Some(data_start) = data_start else {
            return Ok(());
        };
        if data_start % alignment != 0 {
            return Err(format!(
                "Wwise DATA starts at unaligned offset {data_start}"
            ));
        }
        if let Some((_, idx)) = bank.chunks.iter().find(|c| &c.0 == b"DIDX") {
            let mut previous = None;
            for row in idx.chunks_exact(12) {
                let id = u32_at(row, 0)?;
                if previous.is_some_and(|p| p >= id) {
                    return Err("Wwise media index is not strictly sorted".into());
                }
                previous = Some(id);
                if (data_start + u32_at(row, 4)? as usize) % alignment != 0 {
                    return Err("Wwise media address is not aligned".into());
                }
            }
        }
        Ok(())
    }
}

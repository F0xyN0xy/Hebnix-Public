// Reads text chat out of Rocket League's LAN game traffic (UDP, RL_LAN_PORT).
// LAN game packets are not encrypted, so this only watches: a WinDivert
// handle in sniff mode, the same way beacon.rs captures the discovery
// broadcast (needs Hebnix running as administrator). Packets are copied,
// never diverted, sent or modified.
//
// A chat message is a bit-packed (LSB first, not byte aligned) record that
// appears identically in the client -> server RPC and the server -> clients
// broadcast:
//
//   FString  sender id   (int32 len incl NUL, hex account id)
//   int32    always 0 so far
//   FString  message     (int32 len incl NUL; negative len = UTF-16LE)
//   uint64   unix time   (seconds)
//   int32    counter     (per-sender message index)
//
// The channel (global/team) is not part of this record. Quick chats are not
// handled either (they carry no text, only a "GroupNMessageM" id).

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::beacon::{
    RawCapture, WINDIVERT_FLAG_RECV_ONLY, WINDIVERT_FLAG_SNIFF, parse_udp_payload,
};

const MAX_MESSAGE_CHARS: i32 = 512;
// a chat record can't be shorter than id(4+9) + 4 + msg(4+2) + 12
const MIN_PACKET_LEN: usize = 35;
const SEEN_CAPACITY: usize = 256;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ChatMessage {
    /// account id of the sender (hex string, not the display name)
    pub sender: String,
    pub text: String,
    /// unix seconds, as stamped by the sending client
    pub time: u64,
    /// the sender's running message count
    pub counter: i32,
}

pub struct ChatCapture {
    capture: Arc<RawCapture>,
}

impl ChatCapture {
    /// starts the capture thread; `on_message` runs (on that thread) once
    /// per new chat message
    pub fn start(on_message: impl Fn(ChatMessage) + Send + 'static) -> Result<Self, String> {
        let port = super::RL_LAN_PORT;
        // both directions: the sender's RPC goes to the server, the
        // broadcast comes back to every client
        let filter = format!("udp and (udp.SrcPort == {port} or udp.DstPort == {port})");
        let capture = Arc::new(
            RawCapture::open(&filter, WINDIVERT_FLAG_SNIFF | WINDIVERT_FLAG_RECV_ONLY)
                .map_err(|error| format!("could not start the chat capture (WinDivert): {error}"))?,
        );
        let thread_capture = capture.clone();
        std::thread::Builder::new()
            .name("chat-capture".into())
            .spawn(move || {
                let mut buffer = vec![0u8; 65535];
                let mut seen = SeenMessages::default();
                loop {
                    match thread_capture.recv(&mut buffer) {
                        Ok(len) => {
                            let Some((payload, _, _)) = parse_udp_payload(&buffer[..len]) else {
                                continue;
                            };
                            for message in find_chat_messages(&payload, unix_now()) {
                                if seen.is_new(&message) {
                                    on_message(message);
                                }
                            }
                        }
                        // stop()'s shutdown() unblocks recv() with an error,
                        // the normal way out
                        Err(_) => break,
                    }
                }
            })
            .map_err(|error| format!("could not start the chat capture thread: {error}"))?;
        Ok(Self { capture })
    }

    /// stops the capture thread
    pub fn stop(&self) {
        self.capture.shutdown();
    }
}

impl Drop for ChatCapture {
    fn drop(&mut self) {
        self.capture.shutdown();
    }
}

/// every message shows up in the client's RPC, the server's broadcast and any
/// resends, so only the first sighting counts
#[derive(Default)]
struct SeenMessages {
    set: HashSet<(String, u64, i32)>,
    order: VecDeque<(String, u64, i32)>,
}

impl SeenMessages {
    fn is_new(&mut self, message: &ChatMessage) -> bool {
        let key = (message.sender.clone(), message.time, message.counter);
        if !self.set.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > SEEN_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
            }
        }
        true
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// the packet read as a bitstream starting `shift` bits in, LSB first
fn shifted(payload: &[u8], shift: u32) -> Vec<u8> {
    if shift == 0 {
        return payload.to_vec();
    }
    payload
        .windows(2)
        .map(|pair| ((pair[0] >> shift) | (pair[1] << (8 - shift))) as u8)
        .collect()
}

/// (text, position after the string)
fn read_fstring(bytes: &[u8], pos: usize) -> Option<(String, usize)> {
    let length = i32::from_le_bytes(bytes.get(pos..pos + 4)?.try_into().ok()?);
    let start = pos + 4;
    if (1..=MAX_MESSAGE_CHARS).contains(&length) {
        let raw = bytes.get(start..start + length as usize)?;
        let (&last, text) = raw.split_last()?;
        if last != 0 {
            return None;
        }
        let text = String::from_utf8_lossy(text).into_owned();
        if text.chars().any(|c| (c as u32) < 32) {
            return None;
        }
        Some((text, start + length as usize))
    } else if (-MAX_MESSAGE_CHARS..0).contains(&length) {
        let byte_len = (-length) as usize * 2;
        let raw = bytes.get(start..start + byte_len)?;
        let (text, terminator) = raw.split_at(byte_len - 2);
        if terminator != [0, 0] {
            return None;
        }
        let units: Vec<u16> = text
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let text = String::from_utf16(&units).ok()?;
        if text.chars().any(|c| (c as u32) < 32) {
            return None;
        }
        Some((text, start + byte_len))
    } else {
        None
    }
}

fn is_account_id(text: &str) -> bool {
    (8..=64).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn find_chat_messages(payload: &[u8], now: u64) -> Vec<ChatMessage> {
    let mut found = Vec::new();
    if payload.len() < MIN_PACKET_LEN {
        return found;
    }
    let year = 365 * 86_400;
    for shift in 0..8 {
        let bytes = shifted(payload, shift);
        for pos in 0..bytes.len().saturating_sub(4) {
            // account id FString: small length byte, then three zero bytes
            if !(9..=65).contains(&bytes[pos]) || bytes[pos + 1..pos + 4] != [0, 0, 0] {
                continue;
            }
            let Some((sender, after_sender)) = read_fstring(&bytes, pos) else {
                continue;
            };
            if !is_account_id(&sender) {
                continue;
            }
            // 4 bytes that are always zero, then the message
            let Some((text, after_text)) = read_fstring(&bytes, after_sender + 4) else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            let Some(tail) = bytes.get(after_text..after_text + 12) else {
                continue;
            };
            let time = u64::from_le_bytes(tail[..8].try_into().unwrap());
            let counter = i32::from_le_bytes(tail[8..12].try_into().unwrap());
            if time.abs_diff(now) > year {
                continue;
            }
            found.push(ChatMessage {
                sender,
                text,
                time,
                counter,
            });
        }
    }
    found
}

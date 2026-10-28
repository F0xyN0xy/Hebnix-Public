//! Resolve through Windows' configured DNS while bypassing hosts and its
//! contaminated cache. Public DNS is a fallback, not a startup requirement.

use std::collections::HashMap;
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const SERVERS: [&str; 2] = ["8.8.8.8:53", "1.1.1.1:53"];

pub fn resolve_a(host: &str) -> Result<Ipv4Addr, String> {
    type Cache = Mutex<HashMap<String, (Ipv4Addr, Instant)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((ip, expires)) = cache.lock().ok().and_then(|cache| cache.get(host).copied()) {
        if Instant::now() < expires {
            return Ok(ip);
        }
    }
    let (ip, ttl) = resolve_uncached(host)?;
    if let Ok(mut cache) = cache.lock() {
        cache.insert(
            host.to_string(),
            (
                ip,
                Instant::now() + Duration::from_secs(u64::from(ttl.clamp(1, 60))),
            ),
        );
    }
    Ok(ip)
}

fn resolve_uncached(host: &str) -> Result<(Ipv4Addr, u32), String> {
    if let Ok(answer) = windows_dns(host) {
        return Ok(answer);
    }
    let mut last = String::new();
    for server in SERVERS {
        match query(host, server) {
            Ok(ip) if !ip.is_loopback() && !ip.is_unspecified() => return Ok((ip, 60)),
            Ok(_) => last = format!("{server}: DNS returned a local address for {host}"),
            Err(e) => last = format!("{server}: {e}"),
        }
    }
    Err(format!("no A record for {host} ({last})"))
}

fn windows_dns(host: &str) -> Result<(Ipv4Addr, u32), String> {
    #[cfg(target_os = "windows")]
use windows::Win32::NetworkManagement::Dns::*;
    let host: Vec<u16> = host.encode_utf16().chain(Some(0)).collect();
    let mut records = std::ptr::null_mut();
    let result = unsafe {
        DnsQuery_W(
            windows::core::PCWSTR(host.as_ptr()),
            DNS_TYPE_A,
            DNS_QUERY_NO_HOSTS_FILE | DNS_QUERY_BYPASS_CACHE,
            None,
            &mut records,
            None,
        )
    };
    if result.0 != 0 {
        return Err(format!("Windows DNS: {}", result.0));
    }
    let mut record = records;
    let mut answer = None;
    while !record.is_null() {
        let data = unsafe { &*record };
        if data.wType == DNS_TYPE_A.0 {
            let ip = Ipv4Addr::from(unsafe { data.Data.A.IpAddress }.to_ne_bytes());
            if !ip.is_loopback() && !ip.is_unspecified() {
                answer = Some((ip, data.dwTtl));
                break;
            }
        }
        record = data.pNext;
    }
    unsafe {
        DnsFree(Some(records.cast()), DnsFreeRecordList);
    }
    answer.ok_or_else(|| "Windows DNS returned no usable upstream A record".into())
}

fn query(host: &str, server: &str) -> Result<Ipv4Addr, String> {
    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    sock.set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;

    let req = build_query(host)?;
    sock.connect(server).map_err(|e| e.to_string())?;
    sock.send(&req).map_err(|e| e.to_string())?;

    let mut buf = [0u8; 1500];
    let n = sock.recv(&mut buf).map_err(|e| e.to_string())?;
    if n < 12 || buf[..2] != req[..2] || buf[2] & 0x80 == 0 || buf[3] & 0x0f != 0 {
        return Err("Invalid DNS reply".into());
    }
    parse_answer(&buf[..n])
}

fn build_query(host: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(host.len() + 18);
    out.extend_from_slice(&[0x13, 0x37]); // id, one socket so a fixed one is fine
    out.extend_from_slice(&[0x01, 0x00]); // recursion desired
    out.extend_from_slice(&[0x00, 0x01]); // 1 question
    out.extend_from_slice(&[0x00; 6]); // no answer/authority/additional

    for label in host.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(format!("bad label in {host}"));
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&[0x00, 0x01]); // A
    out.extend_from_slice(&[0x00, 0x01]); // IN
    Ok(out)
}

fn parse_answer(buf: &[u8]) -> Result<Ipv4Addr, String> {
    if buf.len() < 12 {
        return Err("short reply".into());
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]);
    let an = u16::from_be_bytes([buf[6], buf[7]]);
    if an == 0 {
        return Err("no answers".into());
    }

    let mut pos = 12;
    for _ in 0..qd {
        pos = skip_name(buf, pos)?;
        pos = pos.checked_add(4).ok_or("truncated question")?;
    }

    for _ in 0..an {
        pos = skip_name(buf, pos)?;
        if pos + 10 > buf.len() {
            return Err("truncated record".into());
        }
        let rtype = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let rdlen = u16::from_be_bytes([buf[pos + 8], buf[pos + 9]]) as usize;
        pos += 10;
        if pos + rdlen > buf.len() {
            return Err("truncated rdata".into());
        }
        // 1 = A. anything else is normally the CNAME sitting in front of it
        if rtype == 1 && rdlen == 4 {
            return Ok(Ipv4Addr::new(
                buf[pos],
                buf[pos + 1],
                buf[pos + 2],
                buf[pos + 3],
            ));
        }
        pos += rdlen;
    }
    Err("no A record in reply".into())
}

/// walk past a name. 0xc0 means its a pointer
fn skip_name(buf: &[u8], mut pos: usize) -> Result<usize, String> {
    loop {
        let len = *buf.get(pos).ok_or("name past end")?;
        if len == 0 {
            return Ok(pos + 1);
        }
        if len & 0xc0 == 0xc0 {
            return Ok(pos + 2);
        }
        pos = pos + 1 + len as usize;
        if pos > buf.len() {
            return Err("name overruns".into());
        }
    }
}

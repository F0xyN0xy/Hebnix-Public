//! The HTTP proxy setting inside Rocket League's Wine prefix.
//!
//! Windows Hebnix points the WinINet system proxy (HKCU Internet Settings) at
//! its HTTP proxy. Under Wine/Proton the same keys live in the prefix's
//! `user.reg`, and Wine's wininet/winhttp read them from there. A running
//! wineserver keeps the registry in memory and writes it back when it exits,
//! so `user.reg` is only edited while nothing runs in the prefix
//! (`hebnix_sdk::process::prefix_in_use`): the setting is applied before
//! Rocket League starts and restored after it has closed.

use std::path::{Path, PathBuf};

const INTERNET_SETTINGS: &str = r"Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings";
const CONNECTIONS: &str =
    r"Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings\\Connections";

/// (registry key as written in user.reg, value name)
const VALUES: [(&str, &str); 4] = [
    (INTERNET_SETTINGS, "ProxyEnable"),
    (INTERNET_SETTINGS, "ProxyServer"),
    (INTERNET_SETTINGS, "ProxyOverride"),
    (CONNECTIONS, "DefaultConnectionSettings"),
];

/// what the four values held before Hebnix changed them: the raw user.reg
/// text after `"Name"=`, None when the value did not exist
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Backup {
    pub prefix: PathBuf,
    pub values: Vec<Option<String>>,
}

pub fn user_reg(prefix: &Path) -> PathBuf {
    prefix.join("user.reg")
}

/// the proxy server string Hebnix writes
pub fn proxy_server(host: &str, port: u16) -> String {
    format!("http={host}:{port};https={host}:{port}")
}

pub fn read_backup(prefix: &Path) -> Result<Backup, String> {
    let text = read(prefix)?;
    Ok(Backup {
        prefix: prefix.to_path_buf(),
        values: VALUES
            .iter()
            .map(|(key, name)| get_value(&text, key, name))
            .collect(),
    })
}

/// points the prefix's proxy at `server`, bypassing it for local addresses
pub fn apply(prefix: &Path, server: &str) -> Result<(), String> {
    let values = [
        Some("dword:00000001".to_string()),
        Some(quote(server)),
        Some(quote("<local>")),
        Some(connection_settings(server, "<local>")),
    ];
    write_values(prefix, &values)
}

pub fn restore(backup: &Backup) -> Result<(), String> {
    if backup.values.len() != VALUES.len() {
        return Err("Malformed Wine proxy backup".into());
    }
    write_values(&backup.prefix, &backup.values)
}

/// does the prefix's proxy currently point at `server`?
#[cfg(test)]
pub fn is_applied(prefix: &Path, server: &str) -> bool {
    read(prefix).is_ok_and(|text| {
        get_value(&text, INTERNET_SETTINGS, "ProxyServer").as_deref() == Some(&quote(server))
    })
}

/// the port of a Hebnix proxy setting (`proxy_server(host, port)`) in the
/// prefix, None when the prefix has no proxy or someone else's
pub fn applied_port(prefix: &Path, host: &str) -> Option<u16> {
    let text = read(prefix).ok()?;
    let value = get_value(&text, INTERNET_SETTINGS, "ProxyServer")?;
    let server = value.strip_prefix('"')?.strip_suffix('"')?;
    let port: u16 = server
        .strip_prefix(&format!("http={host}:"))?
        .split_once(';')?
        .0
        .parse()
        .ok()?;
    (server == proxy_server(host, port)).then_some(port)
}

fn read(prefix: &Path) -> Result<String, String> {
    let path = user_reg(prefix);
    std::fs::read_to_string(&path).map_err(|e| format!("Could not read {}: {e}", path.display()))
}

fn write_values(prefix: &Path, values: &[Option<String>]) -> Result<(), String> {
    let mut text = read(prefix)?;
    for ((key, name), value) in VALUES.iter().zip(values) {
        text = set_value(&text, key, name, value.as_deref());
    }
    let path = user_reg(prefix);
    let temporary = path.with_extension("reg.hebnix-tmp");
    std::fs::write(&temporary, text)
        .and_then(|()| {
            if let Ok(meta) = std::fs::metadata(&path) {
                let _ = std::fs::set_permissions(&temporary, meta.permissions());
            }
            std::fs::rename(&temporary, &path)
        })
        .map_err(|e| {
            let _ = std::fs::remove_file(&temporary);
            format!("Could not write {}: {e}", path.display())
        })
}

fn quote(value: &str) -> String {
    let mut quoted = String::from("\"");
    for character in value.chars() {
        if matches!(character, '"' | '\\') {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted
}

/// WinHTTP's DefaultConnectionSettings blob (what Wine's
/// WinHttpGetIEProxyConfigForCurrentUser reads): magic 0x46, a counter,
/// flags (PROXY_TYPE_DIRECT | PROXY_TYPE_PROXY), then the proxy and bypass
/// strings, each after its length
fn connection_settings(server: &str, bypass: &str) -> String {
    let mut blob = Vec::new();
    for dword in [0x46u32, 0, 3] {
        blob.extend_from_slice(&dword.to_le_bytes());
    }
    for text in [server, bypass] {
        blob.extend_from_slice(&(text.len() as u32).to_le_bytes());
        blob.extend_from_slice(text.as_bytes());
    }
    // no auto-config URL
    blob.extend_from_slice(&0u32.to_le_bytes());
    let bytes: Vec<String> = blob.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("hex:{}", bytes.join(","))
}

/// the line range [start, end) of a key's section (header line included,
/// trailing blank line excluded)
fn section(lines: &[&str], key: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|line| {
        line.strip_prefix('[')
            .and_then(|rest| rest.split_once(']'))
            .is_some_and(|(name, _)| name.eq_ignore_ascii_case(key))
    })?;
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.trim().is_empty() || line.starts_with('['))
        .map_or(lines.len(), |offset| start + 1 + offset);
    Some((start, end))
}

/// the value lines [start, end) of `name` inside a section; a value whose
/// line ends in `\` continues on the next line
fn value_lines(lines: &[&str], (start, end): (usize, usize), name: &str) -> Option<(usize, usize)> {
    let prefix = format!("{}=", quote(name));
    let first = (start + 1..end).find(|&index| {
        lines[index]
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(&prefix))
    })?;
    let mut last = first;
    while last + 1 < end && lines[last].trim_end().ends_with('\\') {
        last += 1;
    }
    Some((first, last + 1))
}

fn get_value(text: &str, key: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let range = section(&lines, key)?;
    let (first, end) = value_lines(&lines, range, name)?;
    let joined = lines[first..end].join("\n");
    Some(joined[quote(name).len() + 1..].to_string())
}

fn set_value(text: &str, key: &str, name: &str, value: Option<&str>) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    let new_line = value.map(|value| format!("{}={value}", quote(name)));
    match section(&borrowed, key) {
        Some(range) => {
            let existing = value_lines(&borrowed, range, name);
            match (existing, new_line) {
                (Some((first, end)), Some(line)) => {
                    lines.splice(first..end, line.lines().map(str::to_string));
                }
                (Some((first, end)), None) => {
                    lines.drain(first..end);
                }
                (None, Some(line)) => {
                    // after the header and its optional #time line
                    let mut at = range.0 + 1;
                    if lines.get(at).is_some_and(|line| line.starts_with("#time=")) {
                        at += 1;
                    }
                    lines.splice(at..at, line.lines().map(str::to_string));
                }
                (None, None) => {}
            }
        }
        None => {
            let Some(line) = new_line else {
                return text.to_string();
            };
            let seconds = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs());
            while lines.last().is_some_and(|line| line.trim().is_empty()) {
                lines.pop();
            }
            lines.push(String::new());
            lines.push(format!("[{key}] {seconds}"));
            lines.extend(line.lines().map(str::to_string));
        }
    }
    let mut output = lines.join("\n");
    output.push('\n');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER_REG: &str = "WINE REGISTRY Version 2\n\
;; All keys relative to \\\\User\\\\S-1-5-21-0-0-0-1000\n\
\n\
#arch=win64\n\
\n\
[Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Internet Settings] 1700000000\n\
#time=1da0000000000000\n\
\"ProxyEnable\"=dword:00000000\n\
\"User Agent\"=\"Mozilla/4.0\"\n\
\n\
[Software\\\\Wine] 1700000000\n\
\"Version\"=\"win10\"\n";

    fn prefix(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hebnix-wine-proxy-{name}-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("user.reg"), USER_REG).unwrap();
        dir
    }

    #[test]
    fn apply_then_restore_gives_back_the_original_registry() {
        let dir = prefix("roundtrip");
        let server = proxy_server("127.0.0.1", 8080);
        let backup = read_backup(&dir).unwrap();
        assert_eq!(
            backup.values,
            vec![Some("dword:00000000".to_string()), None, None, None]
        );
        apply(&dir, &server).unwrap();
        assert!(is_applied(&dir, &server));
        assert_eq!(applied_port(&dir, "127.0.0.1"), Some(8080));
        assert_eq!(applied_port(&dir, "10.0.0.1"), None);
        let text = std::fs::read_to_string(dir.join("user.reg")).unwrap();
        assert!(text.contains("\"ProxyEnable\"=dword:00000001\n"));
        assert!(text.contains("\"ProxyServer\"=\"http=127.0.0.1:8080;https=127.0.0.1:8080\"\n"));
        assert!(text.contains("\"ProxyOverride\"=\"<local>\"\n"));
        assert!(text.contains("\"User Agent\"=\"Mozilla/4.0\"\n"));
        assert!(text.contains("[Software\\\\Wine] 1700000000\n\"Version\"=\"win10\"\n"));
        assert!(text.contains("Internet Settings\\\\Connections] "));
        assert!(text.contains("\"DefaultConnectionSettings\"=hex:46,00,00,00,00,00,00,00,03,00,00,00,"));
        restore(&backup).unwrap();
        assert!(!is_applied(&dir, &server));
        assert_eq!(read_backup(&dir).unwrap().values, backup.values);
        let text = std::fs::read_to_string(dir.join("user.reg")).unwrap();
        assert!(text.starts_with(USER_REG.trim_end()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn multi_line_values_are_replaced_whole() {
        let text = "[A] 1\n\"Blob\"=hex:01,02,\\\n  03,04\n\"Next\"=\"x\"\n";
        assert_eq!(
            get_value(text, "A", "Blob").as_deref(),
            Some("hex:01,02,\\\n  03,04")
        );
        let replaced = set_value(text, "A", "Blob", Some("hex:05"));
        assert_eq!(replaced, "[A] 1\n\"Blob\"=hex:05\n\"Next\"=\"x\"\n");
        let removed = set_value(text, "A", "blob", None);
        assert_eq!(removed, "[A] 1\n\"Next\"=\"x\"\n");
    }

    #[test]
    fn connection_settings_layout() {
        let blob = connection_settings("p", "b");
        assert_eq!(
            blob,
            "hex:46,00,00,00,00,00,00,00,03,00,00,00,01,00,00,00,70,01,00,00,00,62,00,00,00,00"
        );
    }
}

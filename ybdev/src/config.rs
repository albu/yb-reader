//! mirror.conf handling, ported 1:1 from mirror.koplugin's readServerConf /
//! writeServerConf / parseServer / sanitizeFetchName / urldecode.

#[derive(Debug, Clone, Default)]
pub struct ServerConf {
    pub server: Option<String>,
    pub refresh_every: Option<u32>,
    /// Read-mode page-turn keys: "arrows" (default), "space"
    /// (Space / Shift+Space) or "pages" (PageDown / PageUp).
    pub turn_keys: Option<String>,
    /// Optional shared credential (`SECRET=`): sent as `X-YB-Secret` on
    /// every mirror / ai_stream request. Only means something once the
    /// server requires the header; absent keeps the wire identical to
    /// the unauthenticated protocol.
    pub secret: Option<String>,
}

pub fn read(path: &str) -> ServerConf {
    let mut conf = ServerConf::default();
    let Ok(text) = std::fs::read_to_string(path) else {
        return conf;
    };
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("SERVER=") {
            let v = rest.trim();
            if !v.is_empty() {
                conf.server = Some(v.to_string());
            }
        } else if let Some(rest) = t.strip_prefix("REFRESH_EVERY=") {
            if let Ok(n) = rest.trim().parse::<u32>() {
                conf.refresh_every = Some(n);
            }
        } else if let Some(rest) = t.strip_prefix("TURN_KEYS=") {
            let v = rest.trim();
            if !v.is_empty() {
                conf.turn_keys = Some(v.to_string());
            }
        } else if let Some(rest) = t.strip_prefix("SECRET=") {
            let v = rest.trim();
            if !v.is_empty() {
                conf.secret = Some(v.to_string());
            }
        }
    }
    conf
}

/// Rewrite the conf file, preserving every non-SERVER line and appending
/// `SERVER=...` (matches writeServerConf).
pub fn write_server(path: &str, server: &str) {
    let mut lines: Vec<String> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            if !line.trim_start().starts_with("SERVER=") {
                lines.push(line.to_string());
            }
        }
    }
    lines.push(format!("SERVER={}", server));
    if let Err(e) = std::fs::write(path, lines.join("\n") + "\n") {
        // A silent drop here would make the user's pinned server
        // "forget" on every launch (discovery rewrites it each run).
        crate::log::plog(&format!("conf: failed to write SERVER to {}: {}", path, e));
    }
}

/// Rewrite the conf file, preserving every non-TURN_KEYS line and
/// appending `TURN_KEYS=...` — the settings sheet's preset picker owns
/// this line, the discovery flow owns SERVER (write_server), and the two
/// writers must not clobber each other.
pub fn write_turn_keys(path: &str, preset: &str) {
    let mut lines: Vec<String> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            if !line.trim_start().starts_with("TURN_KEYS=") {
                lines.push(line.to_string());
            }
        }
    }
    lines.push(format!("TURN_KEYS={}", preset));
    if let Err(e) = std::fs::write(path, lines.join("\n") + "\n") {
        crate::log::plog(&format!("conf: failed to write TURN_KEYS to {}: {}", path, e));
    }
}

/// "http://192.0.2.1:8765" / "192.0.2.1:8765" / "192.0.2.1" /
/// "mybook.local:8765" -> (host, port). Port defaults to 8765.
pub fn parse_server(s: &str) -> (Option<String>, u16) {
    let mut s = s.trim();
    if let Some(rest) = s.strip_prefix("http://") {
        s = rest;
    }
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_string(), p.parse::<u16>().unwrap_or(8765))
        }
        _ => (s.to_string(), 8765),
    };
    if host.is_empty() {
        (None, 8765)
    } else {
        (Some(host), port)
    }
}

/// %XX -> byte; byte-transparent for UTF-8 names.
///
/// '+' is intentionally left alone: every client we serve (QR URL, upload
/// page) builds queries with `encodeURIComponent`, which emits `%20` for
/// spaces and `%2B` for plus. Mapping '+' to space here only corrupted
/// manually-typed names like `name=c++.pdf`.
pub fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_val(bytes[i + 1]);
                let lo = hex_val(bytes[i + 2]);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h << 4) | l);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Basename only, no control bytes, nothing hidden/dot-like left.
pub fn sanitize_fetch_name(name: &str) -> Option<String> {
    let name = name.replace('\\', "/");
    let name = name.rsplit('/').next().unwrap_or("");
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    let name = name.trim_start_matches('.');
    if name.is_empty() || name == "." || name == ".." {
        None
    } else {
        Some(name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn turn_keys_and_server_writers_preserve_each_other() {
        let path = std::env::temp_dir().join(format!("mirror_conf_{}.txt", std::process::id()));
        let path = path.to_str().unwrap();
        let _ = fs::remove_file(path);
        fs::write(
            path,
            "SERVER=http://192.0.2.1:8765\nREFRESH_EVERY=30\nTURN_KEYS=space\n",
        )
        .unwrap();

        // The sheet picks a new preset: SERVER and REFRESH_EVERY survive.
        write_turn_keys(path, "pages");
        let conf = read(path);
        assert_eq!(conf.server.as_deref(), Some("http://192.0.2.1:8765"));
        assert_eq!(conf.refresh_every, Some(30));
        assert_eq!(conf.turn_keys.as_deref(), Some("pages"));

        // Discovery then rewrites SERVER: the picked preset survives.
        write_server(path, "http://192.168.1.9:8765");
        let conf = read(path);
        assert_eq!(conf.server.as_deref(), Some("http://192.168.1.9:8765"));
        assert_eq!(conf.turn_keys.as_deref(), Some("pages"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn secret_is_read_and_survives_both_writers() {
        let path = std::env::temp_dir().join(format!("mirror_conf_sec_{}.txt", std::process::id()));
        let path = path.to_str().unwrap();
        let _ = fs::remove_file(path);
        fs::write(
            path,
            "SERVER=http://192.0.2.1:8765\nSECRET=s3cret\nTURN_KEYS=pages\n",
        )
        .unwrap();
        assert_eq!(read(path).secret.as_deref(), Some("s3cret"));

        // Neither writer owns SECRET: both must preserve it.
        write_turn_keys(path, "arrows");
        write_server(path, "http://192.168.1.9:8765");
        let conf = read(path);
        assert_eq!(conf.secret.as_deref(), Some("s3cret"));
        assert_eq!(conf.turn_keys.as_deref(), Some("arrows"));
        assert_eq!(conf.server.as_deref(), Some("http://192.168.1.9:8765"));

        // Empty / missing line means no credential.
        fs::write(path, "SERVER=http://x:1\nSECRET=\n").unwrap();
        assert!(read(path).secret.is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn write_turn_keys_creates_the_file_when_absent() {
        let path =
            std::env::temp_dir().join(format!("mirror_conf_absent_{}.txt", std::process::id()));
        let path = path.to_str().unwrap();
        let _ = fs::remove_file(path);

        write_turn_keys(path, "arrows");
        assert_eq!(read(path).turn_keys.as_deref(), Some("arrows"));
        let _ = fs::remove_file(path);
    }
}

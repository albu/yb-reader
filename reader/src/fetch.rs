//! Fetch book from Mac — port of mirror.koplugin's _fetchBook.
//! The file counts as received only after the full body is read *and*
//! renamed into place; only then does /ack go out.

use std::time::Duration;

use ybdev::config::{self, sanitize_fetch_name, urldecode};
use ybdev::log::{now_ms, plog};

use crate::protocol::{self, Conn};
use crate::wifi;

const CONF_PATH: &str = "/mnt/us/extensions/mirror/mirror.conf";
const SAVE_DIR: &str = "/mnt/us/documents";

pub fn fetch_book() -> Result<String, String> {
    wifi::ensure_wifi();

    // Candidate addresses, most likely first: send.py next to the saved
    // mirror address, send.py at a freshly discovered Mac, and whatever
    // port discovery reported last.
    let mut cands: Vec<(String, u16)> = Vec::new();
    let conf = config::read(CONF_PATH);
    if let Some(s) = conf.server {
        let (host, _port) = config::parse_server(&s);
        if let Some(h) = host {
            cands.push((h, protocol::FETCH_PORT));
        }
    }
    if let Some((ip, port)) = protocol::discover(Duration::from_secs(1)) {
        cands.push((ip.clone(), protocol::FETCH_PORT));
        cands.push((ip, port));
    }

    let mut seen = std::collections::HashSet::new();
    for (host, port) in cands {
        let key = format!("{}:{}", host, port);
        if !seen.insert(key.clone()) {
            continue;
        }
        match fetch_from(&host, port) {
            Ok(msg) => return Ok(msg),
            Err(e) => plog(&format!("fetch: {} — {}", key, e)),
        }
    }
    Err("Mac not found — run send.py there first".to_string())
}

fn fetch_from(host: &str, port: u16) -> Result<String, String> {
    let mut conn = Conn::new(host, port);
    if !conn.open() {
        return Err("connect".to_string());
    }

    // /status also exists on the mirror server (different JSON): without
    // file+size this isn't send.py, keep looking.
    let mut body: Vec<u8> = Vec::new();
    {
        let mut sink = |c: &[u8]| {
            body.extend_from_slice(c);
            true
        };
        let r = conn
            .request("GET", "/status", &mut sink)
            .map_err(|_| "no status")?;
        if r.status != 200 {
            return Err("no status".to_string());
        }
    }
    let text = String::from_utf8_lossy(&body).into_owned();
    let name = json_field(&text, "file");
    let size = json_field(&text, "size")
        .and_then(|s| s.parse::<u64>().ok());
    let (Some(name), Some(size)) = (name, size) else {
        return Err("not send.py".to_string());
    };
    let name = sanitize_fetch_name(&urldecode(&name)).ok_or("bad name")?;
    let final_path = format!("{}/{}", SAVE_DIR, name);
    let part_path = format!("{}.part", final_path);
    let t0 = now_ms();

    let mut status: Option<u16> = None;
    for _ in 0..2 {
        let mut out = std::fs::File::create(&part_path)
            .map_err(|e| format!("cannot write: {}", e))?;
        {
            let mut sink = |c: &[u8]| {
                let _ = std::io::Write::write_all(&mut out, c);
                true
            };
            let r = conn.request("GET", "/book", &mut sink).ok();
            status = r.map(|r| r.status);
        }
        drop(out);
        if status == Some(200) {
            break;
        }
        let _ = std::fs::remove_file(&part_path);
        if status == Some(410) {
            return Err("already delivered".to_string());
        }
    }
    if status != Some(200) {
        return Err("download failed".to_string());
    }
    if std::fs::rename(&part_path, &final_path).is_err() {
        return Err("rename failed".to_string());
    }

    let mut sink = |_: &[u8]| true;
    let _ = conn.request("GET", "/ack", &mut sink);
    let secs = (now_ms() - t0) as f64 / 1000.0;
    plog(&format!(
        "fetch: saved {} ({} B) from {}:{} in {:.1}s",
        name, size, host, port, secs
    ));
    Ok(format!(
        "Saved to documents:\n{} ({:.1} MB)",
        name,
        size as f64 / 1048576.0
    ))
}

/// Pull `"key" : value` from the send.py status JSON (string or number).
fn json_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let i = text.find(&needle)?;
    let rest = &text[i + needle.len()..];
    let j = rest.find(':')?;
    let v = rest[j + 1..].trim();
    if let Some(s) = v.strip_prefix('"') {
        let e = s.find('"')?;
        Some(s[..e].to_string())
    } else {
        let e = v
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(v.len());
        Some(v[..e].to_string())
    }
}

//! Trusted devices store and Kindle identity profile for yb-reader.
//!
//! Stores paired laptops/phones with their per-device authorization tokens,
//! friendly names, and last-seen IP addresses. Also maintains the Kindle's
//! own identity (ID, friendly name, screen dimensions).

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_KINDLE_ID_PATH: &str = "/var/local/yb-reader/kindle_id.json";
pub const FALLBACK_KINDLE_ID_PATH: &str = "/mnt/us/extensions/mirror/kindle_id.json";
pub const DEFAULT_DEVICES_PATH: &str = "/var/local/yb-reader/devices.json";
pub const FALLBACK_DEVICES_PATH: &str = "/mnt/us/extensions/mirror/devices.json";

/// Helper to get the devices store path from environment or default.
/// Prefers private internal storage (/var/local/yb-reader/) to protect
/// device credentials from USB mass-storage exposure, falling back to
/// /mnt/us/extensions/mirror/ if /var/local is unavailable.
/// The env override is checked on every call (tests swap it); only the
/// default resolution is cached per process.
pub fn devices_path() -> String {
    if let Ok(p) = std::env::var("YB_DEVICES_PATH") {
        return p;
    }
    default_paths().0.clone()
}

/// Helper to get the kindle ID path from environment or default.
pub fn kindle_id_path() -> String {
    if let Ok(p) = std::env::var("YB_KINDLE_ID_PATH") {
        return p;
    }
    default_paths().1.clone()
}

/// Resolve the default (devices, kindle_id) paths once per process.
/// The first resolution on private storage runs a one-time migration from
/// the legacy /mnt/us location so existing pairings and the Kindle identity
/// survive the move — and the USB-visible copies are retired.
fn default_paths() -> &'static (String, String) {
    static DEFAULTS: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    DEFAULTS.get_or_init(|| {
        if std::path::Path::new("/var/local").exists() {
            migrate_legacy_file(FALLBACK_DEVICES_PATH, DEFAULT_DEVICES_PATH);
            migrate_legacy_file(FALLBACK_KINDLE_ID_PATH, DEFAULT_KINDLE_ID_PATH);
            (
                DEFAULT_DEVICES_PATH.to_string(),
                DEFAULT_KINDLE_ID_PATH.to_string(),
            )
        } else {
            (
                FALLBACK_DEVICES_PATH.to_string(),
                FALLBACK_KINDLE_ID_PATH.to_string(),
            )
        }
    })
}

/// One-time migration of a legacy store/profile file to the private
/// location. Rename is preferred (it retires the USB-visible original in
/// one step); copy+remove covers cross-filesystem moves.
fn migrate_legacy_file(from: &str, to: &str) {
    if std::path::Path::new(to).exists() || !std::path::Path::new(from).exists() {
        return;
    }
    if fs::rename(from, to).is_ok() {
        return;
    }
    if let Ok(data) = fs::read(from) {
        if save_secure(to, &data).is_ok() {
            let _ = fs::remove_file(from);
        }
    }
}

static STORE_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Safely load, modify, and save the DeviceStore at a specific path under a process-wide mutex.
/// Guarantees that every mutation reads the latest state from disk and persists
/// atomically without racing other threads or resurrecting stale records.
/// Aborts and quarantines if devices.json exists but is unreadable/corrupt to prevent
/// silent erasure.
pub fn with_store_mut_at<R>(path: &str, f: impl FnOnce(&mut DeviceStore) -> R) -> std::io::Result<R> {
    let _guard = STORE_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = load_for_mutation(path)?;
    let res = f(&mut store);
    store.save(path)?;
    Ok(res)
}

/// Safely load, modify, and save the DeviceStore at devices_path() under a process-wide mutex.
pub fn with_store_mut<R>(f: impl FnOnce(&mut DeviceStore) -> R) -> std::io::Result<R> {
    with_store_mut_at(&devices_path(), f)
}

/// Load the store for an in-place mutation, quarantining corrupt files so
/// the caller can abort instead of erasing paired devices.
fn load_for_mutation(path: &str) -> std::io::Result<DeviceStore> {
    match DeviceStore::load_result(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            let bad_path = format!("{}.corrupt.{}", path, std::process::id());
            let _ = fs::rename(path, &bad_path);
            Err(e)
        }
        Err(e) => Err(e),
    }
}

/// Refresh a device's last_ip/last_seen under the store mutex, persisting
/// ONLY when the IP actually changed. This runs on every authenticated
/// request, so an unchanged IP must not cost a flash write cycle.
pub fn refresh_device_ip(path: &str, token: &str, ip: &str, timestamp: u64) -> std::io::Result<bool> {
    let _guard = STORE_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = load_for_mutation(path)?;
    let changed = store.update_ip_and_seen_by_token(token, ip, timestamp);
    if changed {
        store.save(path)?;
    }
    Ok(changed)
}

/// Atomically and durably write a private file with 0o600 permissions.
pub fn save_secure(path: &str, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.exists() && fs::create_dir_all(parent).is_ok() {
            // Tighten only directories we just created (never pre-existing
            // system dirs) so the 0o600 file isn't listable in a 0755 dir.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            }
        }
    }
    let tmp = format!("{}.{}.tmp", path, std::process::id());
    let mut f = fs::File::create(&tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = f.set_permissions(fs::Permissions::from_mode(0o600));
    }
    use std::io::Write;
    f.write_all(data)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)?;
    Ok(())
}

/// The Kindle's own hardware and identity profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindleProfile {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub bpp: u8,
}

impl KindleProfile {
    pub const DEFAULT_PW5_W: u32 = 1236;
    pub const DEFAULT_PW5_H: u32 = 1648;

    pub fn default_pw5(id: &str) -> KindleProfile {
        KindleProfile {
            id: id.to_string(),
            name: "Kindle Paperwhite".to_string(),
            width: Self::DEFAULT_PW5_W,
            height: Self::DEFAULT_PW5_H,
            bpp: 4,
        }
    }

    /// Load from JSON file, or generate a new profile and save it if missing.
    pub fn load_or_create(path: &str, default_w: u32, default_h: u32) -> KindleProfile {
        if let Ok(content) = fs::read_to_string(path) {
            if let Some(profile) = Self::from_json(&content) {
                return profile;
            }
        }

        // Generate a stable Kindle ID based on /dev/urandom or clock
        let id = format!("knd_{}", generate_random_hex(6));
        let profile = KindleProfile {
            id,
            name: "Kindle Paperwhite".to_string(),
            width: default_w,
            height: default_h,
            bpp: 4,
        };
        let _ = profile.save(path);
        profile
    }

    pub fn from_json(json: &str) -> Option<KindleProfile> {
        let id = extract_json_str(json, "id")?;
        let name = extract_json_str(json, "name").unwrap_or_else(|| "Kindle".to_string());
        let width = extract_json_u64(json, "width").unwrap_or(Self::DEFAULT_PW5_W as u64) as u32;
        let height = extract_json_u64(json, "height").unwrap_or(Self::DEFAULT_PW5_H as u64) as u32;
        let bpp = extract_json_u64(json, "bpp").unwrap_or(4) as u8;

        Some(KindleProfile {
            id,
            name,
            width,
            height,
            bpp,
        })
    }

    pub fn to_json(&self) -> String {
        format!(
            "{{\n  \"id\": \"{}\",\n  \"name\": \"{}\",\n  \"width\": {},\n  \"height\": {},\n  \"bpp\": {}\n}}",
            json_escape(&self.id),
            json_escape(&self.name),
            self.width,
            self.height,
            self.bpp
        )
    }

    pub fn save(&self, path: &str) -> std::io::Result<()> {
        save_secure(path, self.to_json().as_bytes())
    }
}

/// A trusted laptop, phone, or tablet authorized to connect to this Kindle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedDevice {
    pub id: String,
    pub name: String,
    pub token: String,
    pub last_ip: Option<String>,
    pub last_seen: u64,
    /// "inbound" (file copy only) or "all" (file copy + mirror + stream control)
    pub scope: String,
}

impl TrustedDevice {
    pub fn new(id: &str, name: &str, token: &str, ip: Option<&str>, scope: &str) -> TrustedDevice {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        TrustedDevice {
            id: id.to_string(),
            name: name.to_string(),
            token: token.to_string(),
            last_ip: ip.map(|s| s.to_string()),
            last_seen: now,
            scope: if scope.is_empty() { "all".to_string() } else { scope.to_string() },
        }
    }
}

/// Persistent store of trusted client devices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceStore {
    pub devices: Vec<TrustedDevice>,
}

impl DeviceStore {
    pub fn load(path: &str) -> DeviceStore {
        Self::load_result(path).unwrap_or_default()
    }

    pub fn load_result(path: &str) -> std::io::Result<DeviceStore> {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DeviceStore::default());
            }
            Err(e) => return Err(e),
        };
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Ok(DeviceStore::default());
        }
        if !store_json_is_complete(trimmed) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "devices.json is truncated or not valid JSON",
            ));
        }
        Ok(Self::from_json(trimmed))
    }

    pub fn from_json(json: &str) -> DeviceStore {
        let mut devices = Vec::new();
        let mut rest = json.trim();
        while let Some(start_obj) = rest.find('{') {
            rest = &rest[start_obj + 1..];
            let Some(end_obj) = find_closing_brace(rest) else {
                break;
            };
            let obj_str = &rest[..end_obj];
            rest = &rest[end_obj + 1..];

            if let Some(id) = extract_json_str(obj_str, "id") {
                if let Some(token) = extract_json_str(obj_str, "token") {
                    let name = extract_json_str(obj_str, "name").unwrap_or_else(|| "Device".to_string());
                    let last_ip = extract_json_str(obj_str, "last_ip");
                    let last_seen = extract_json_u64(obj_str, "last_seen").unwrap_or(0);
                    let scope = extract_json_str(obj_str, "scope").unwrap_or_else(|| "all".to_string());

                    devices.push(TrustedDevice {
                        id,
                        name,
                        token,
                        last_ip,
                        last_seen,
                        scope,
                    });
                }
            }
        }
        DeviceStore { devices }
    }

    pub fn to_json(&self) -> String {
        let mut out = String::from("[\n");
        for (i, d) in self.devices.iter().enumerate() {
            out.push_str("  {\n");
            out.push_str(&format!("    \"id\": \"{}\",\n", json_escape(&d.id)));
            out.push_str(&format!("    \"name\": \"{}\",\n", json_escape(&d.name)));
            out.push_str(&format!("    \"token\": \"{}\",\n", json_escape(&d.token)));
            if let Some(ip) = &d.last_ip {
                out.push_str(&format!("    \"last_ip\": \"{}\",\n", json_escape(ip)));
            } else {
                out.push_str("    \"last_ip\": null,\n");
            }
            out.push_str(&format!("    \"last_seen\": {},\n", d.last_seen));
            out.push_str(&format!("    \"scope\": \"{}\"\n", json_escape(&d.scope)));
            out.push_str("  }");
            if i + 1 < self.devices.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push(']');
        out
    }

    pub fn save(&self, path: &str) -> std::io::Result<()> {
        save_secure(path, self.to_json().as_bytes())
    }

    pub fn find_by_token(&self, token: &str) -> Option<&TrustedDevice> {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            return None;
        }
        // Constant-time compare: these are never-rotating credentials and
        // the PIN path already uses one — a plain == early-exit would leak
        // prefix matches to a patient LAN attacker.
        self.devices.iter().find(|d| const_time_eq(&d.token, trimmed))
    }

    pub fn find_by_token_for_inbound(&self, token: &str) -> Option<&TrustedDevice> {
        let d = self.find_by_token(token)?;
        if d.scope == "inbound" || d.scope == "all" {
            Some(d)
        } else {
            None
        }
    }

    pub fn find_by_id(&self, id: &str) -> Option<&TrustedDevice> {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            return None;
        }
        self.devices.iter().find(|d| d.id == trimmed)
    }

    pub fn find_by_id_for_control(&self, id: &str) -> Option<&TrustedDevice> {
        let d = self.find_by_id(id)?;
        if d.scope == "all" || d.scope == "control" || d.scope == "mirror" {
            Some(d)
        } else {
            None
        }
    }

    pub fn find_by_ip(&self, ip: &str) -> Option<&TrustedDevice> {
        let trimmed = ip.trim();
        if trimmed.is_empty() {
            return None;
        }
        self.devices.iter().find(|d| d.last_ip.as_deref() == Some(trimmed))
    }

    pub fn find_by_ip_for_control(&self, ip: &str) -> Option<&TrustedDevice> {
        let d = self.find_by_ip(ip)?;
        if d.scope == "all" || d.scope == "control" || d.scope == "mirror" {
            Some(d)
        } else {
            None
        }
    }

    pub fn add_or_update(&mut self, device: TrustedDevice) {
        if let Some(pos) = self.devices.iter().position(|d| d.id == device.id) {
            self.devices[pos] = device;
        } else {
            self.devices.push(device);
        }
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let len_before = self.devices.len();
        self.devices.retain(|d| d.id != id);
        self.devices.len() < len_before
    }

    pub fn update_ip_and_seen_by_token(&mut self, token: &str, ip: &str, timestamp: u64) -> bool {
        let trimmed = token.trim();
        if let Some(d) = self.devices.iter_mut().find(|d| d.token == trimmed) {
            let changed = d.last_ip.as_deref() != Some(ip);
            d.last_ip = Some(ip.to_string());
            d.last_seen = timestamp;
            return changed;
        }
        false
    }
}

/// Fill a slice with cryptographically secure random bytes from /dev/urandom with fallback.
pub fn fill_random_bytes(buf: &mut [u8]) {
    use std::io::Read;
    if let Ok(mut f) = fs::File::open("/dev/urandom") {
        if f.read_exact(buf).is_ok() {
            return;
        }
    }
    // High-entropy fallback if /dev/urandom read is short/unavailable
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let ptr = buf.as_ptr() as usize;
    let mut state = now ^ ((pid as u128) << 48) ^ ((ptr as u128) << 16);
    for b in buf.iter_mut() {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *b = (state >> 56) as u8;
    }
}

/// Generate a high-entropy hex token (e.g. 32 chars / 128 bits) for a paired device.
pub fn generate_token() -> String {
    format!("tok_{}", generate_random_hex(16))
}

/// Generate a random hex string of `n_bytes` (output is `2 * n_bytes` chars).
pub fn generate_random_hex(n_bytes: usize) -> String {
    let mut buf = vec![0u8; n_bytes];
    fill_random_bytes(&mut buf);
    let mut hex = String::with_capacity(n_bytes * 2);
    for b in buf {
        use std::fmt::Write;
        let _ = write!(hex, "{:02x}", b);
    }
    hex
}

pub fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

pub fn extract_json_str(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":", key);
    let idx = json.find(&pattern)?;
    let rest = json[idx + pattern.len()..].trim_start();
    if rest.starts_with("null") {
        return None;
    }
    if !rest.starts_with('"') {
        return None;
    }
    let chars: Vec<char> = rest[1..].chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if escaped {
            match c {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'u' => {
                    // \uXXXX escape, including surrogate pairs for non-BMP
                    // code points (e.g. emoji in device names).
                    i = decode_unicode_escape(&chars, i, &mut out);
                }
                _ => {
                    out.push('\\');
                    out.push(c);
                }
            }
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return Some(out);
        } else {
            out.push(c);
        }
        i += 1;
    }
    Some(out)
}

/// Decode a `\uXXXX` escape (and a following low surrogate when the first
/// code unit is a high surrogate) starting at the 'u'. Returns the index of
/// the last consumed char. Malformed escapes push U+FFFD rather than fail.
fn decode_unicode_escape(chars: &[char], u_idx: usize, out: &mut String) -> usize {
    let parse_hex = |from: usize| -> Option<u32> {
        if from + 4 > chars.len() {
            return None;
        }
        let hex: String = chars[from..from + 4].iter().collect();
        u32::from_str_radix(&hex, 16).ok()
    };
    let Some(cp) = parse_hex(u_idx + 1) else {
        // Truncated escape (\u12 then end/quote): swallow the rest — the
        // leftover hex digits must not leak through as literal text.
        out.push('\u{FFFD}');
        return chars.len().saturating_sub(1).max(u_idx);
    };
    let i = u_idx + 4; // last hex digit of the first escape
    if (0xD800..=0xDBFF).contains(&cp) {
        // High surrogate: combine with a following \uXXXX low surrogate.
        if chars.get(i + 1) == Some(&'\\') && chars.get(i + 2) == Some(&'u') {
            if let Some(low) = parse_hex(i + 3) {
                if (0xDC00..=0xDFFF).contains(&low) {
                    let combined = 0x10000 + ((cp - 0xD800) << 10) + (low - 0xDC00);
                    out.push(char::from_u32(combined).unwrap_or('\u{FFFD}'));
                    return i + 6; // last hex digit of the low escape
                }
            }
        }
        out.push('\u{FFFD}');
    } else {
        out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
    }
    i
}

pub fn extract_json_u64(json: &str, key: &str) -> Option<u64> {
    let pattern = format!("\"{}\":", key);
    let idx = json.find(&pattern)?;
    let rest = json[idx + pattern.len()..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse::<u64>().ok()
}

/// Constant-time equality for secrets. Length differences short-circuit
/// (length is not secret here); equal-length comparisons always scan fully.
fn const_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Structural completeness check for the store file: every object must have
/// a matching closing brace, and the top-level array (or single object) must
/// be closed with only whitespace after it. Catches truncated files (power
/// loss, flash wear) that a first-character sniff would wave through and
/// that from_json would otherwise silently load as a partial store.
fn store_json_is_complete(trimmed: &str) -> bool {
    let mut rest = trimmed;
    if let Some(after) = rest.strip_prefix('[') {
        rest = after.trim_start();
        loop {
            if let Some(after_close) = rest.strip_prefix(']') {
                return after_close.trim().is_empty();
            }
            let Some(start_obj) = rest.find('{') else {
                return false;
            };
            let inner = &rest[start_obj + 1..];
            let Some(end_obj) = find_closing_brace(inner) else {
                return false; // truncated mid-object
            };
            rest = inner[end_obj + 1..].trim_start();
            if let Some(after_comma) = rest.strip_prefix(',') {
                rest = after_comma.trim_start();
            }
        }
    } else if let Some(after) = rest.strip_prefix('{') {
        match find_closing_brace(after) {
            Some(end) => after[end + 1..].trim().is_empty(),
            None => false,
        }
    } else {
        false
    }
}

fn find_closing_brace(s: &str) -> Option<usize> {
    let mut depth = 1;
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
        } else {
            match c {
                '"' => in_str = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kindle_profile_roundtrip() {
        let profile = KindleProfile {
            id: "knd_test_123".to_string(),
            name: "Bedside Oasis".to_string(),
            width: 1072,
            height: 1448,
            bpp: 4,
        };
        let json = profile.to_json();
        let parsed = KindleProfile::from_json(&json).unwrap();
        assert_eq!(profile, parsed);
    }

    #[test]
    fn device_store_add_find_and_remove() {
        let mut store = DeviceStore::default();
        let dev1 = TrustedDevice::new(
            "mbp_1",
            "MacBook Pro",
            "tok_sec_1234567890abcdef",
            Some("192.168.1.10"),
            "all",
        );
        let dev2 = TrustedDevice::new(
            "thinkpad_work",
            "Work ThinkPad",
            "tok_sec_0987654321fedcba",
            Some("192.168.1.20"),
            "inbound",
        );

        store.add_or_update(dev1.clone());
        store.add_or_update(dev2.clone());

        assert_eq!(store.devices.len(), 2);
        assert_eq!(store.find_by_token("tok_sec_1234567890abcdef"), Some(&dev1));
        assert_eq!(store.find_by_id("thinkpad_work"), Some(&dev2));

        // Scope checks: dev1 has "all", dev2 has "inbound"
        assert_eq!(store.find_by_token_for_inbound("tok_sec_1234567890abcdef"), Some(&dev1));
        assert_eq!(store.find_by_token_for_inbound("tok_sec_0987654321fedcba"), Some(&dev2));
        assert_eq!(store.find_by_id_for_control("mbp_1"), Some(&dev1));
        assert_eq!(store.find_by_id_for_control("thinkpad_work"), None); // "inbound" scope is not allowed for control
        assert_eq!(store.find_by_ip_for_control("192.168.1.10"), Some(&dev1));
        assert_eq!(store.find_by_ip_for_control("192.168.1.20"), None);

        // IP refresh via token
        assert!(store.update_ip_and_seen_by_token("tok_sec_1234567890abcdef", "192.168.1.42", 1000));
        assert_eq!(store.find_by_id("mbp_1").unwrap().last_ip.as_deref(), Some("192.168.1.42"));
        assert!(!store.update_ip_and_seen_by_token("tok_sec_1234567890abcdef", "192.168.1.42", 1001)); // unchanged

        let json = store.to_json();
        let loaded = DeviceStore::from_json(&json);
        assert_eq!(store, loaded);

        // Test save_secure permissions
        let test_path = std::env::temp_dir().join(format!("yb_test_sec_store_{}.json", std::process::id()));
        store.save(test_path.to_str().unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(&test_path).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_file(test_path);

        assert!(store.remove("mbp_1"));
        assert_eq!(store.devices.len(), 1);
        assert_eq!(store.find_by_id("mbp_1"), None);
    }

    #[test]
    fn robust_token_generation() {
        let t1 = generate_token();
        let t2 = generate_token();
        assert!(t1.starts_with("tok_"));
        assert!(t2.starts_with("tok_"));
        assert_ne!(t1, t2);
        assert_eq!(t1.len(), 4 + 32); // "tok_" + 32 hex chars
    }

    #[test]
    fn extract_json_str_decodes_unicode_escapes() {
        // BMP escapes + an emoji as a surrogate pair, all via \u escapes.
        let json = r#"{"name": "café ☃ 😀 end"}"#;
        assert_eq!(extract_json_str(json, "name").unwrap(), "café ☃ 😀 end");
        // Lone high surrogate and truncated escapes degrade to U+FFFD
        // without leaking the raw hex digits as literal text.
        assert_eq!(extract_json_str(r#"{"n": "\ud83d"}"#, "n").unwrap(), "\u{FFFD}");
        assert_eq!(extract_json_str(r#"{"n": "\u12"}"#, "n").unwrap(), "\u{FFFD}");
        // Round-trip through to_json keeps raw UTF-8 (no escapes emitted).
        let d = TrustedDevice::new("d1", "café ☃ 😀", "tok_1", None, "all");
        let loaded = DeviceStore::from_json(&DeviceStore { devices: vec![d] }.to_json());
        assert_eq!(loaded.devices[0].name, "café ☃ 😀");
    }

    #[test]
    fn test_with_store_mut_serialization() {
        let test_path = std::env::temp_dir().join(format!("yb_test_with_store_mut_{}.json", std::process::id()));
        let test_str = test_path.to_str().unwrap();

        // Initialize with dev1
        let _ = with_store_mut_at(test_str, |s| {
            s.add_or_update(TrustedDevice::new("d1", "Dev 1", "tok_1", None, "inbound"));
        });
        assert_eq!(DeviceStore::load(test_str).devices.len(), 1);

        // Add dev2
        let _ = with_store_mut_at(test_str, |s| {
            s.add_or_update(TrustedDevice::new("d2", "Dev 2", "tok_2", None, "all"));
        });
        assert_eq!(DeviceStore::load(test_str).devices.len(), 2);

        // Revoke all
        let _ = with_store_mut_at(test_str, |s| {
            s.devices.clear();
        });
        assert_eq!(DeviceStore::load(test_str).devices.len(), 0);

        let _ = std::fs::remove_file(&test_path);
    }

    #[test]
    fn test_corrupt_file_quarantine_in_with_store_mut() {
        let test_path = std::env::temp_dir().join(format!("yb_test_corrupt_store_{}.json", std::process::id()));
        let test_str = test_path.to_str().unwrap();

        // Write invalid/corrupted JSON garbage
        std::fs::write(&test_path, b"GARBAGE NON JSON DATA").unwrap();

        // with_store_mut_at should detect corruption, abort without writing empty store, and quarantine the file
        let res = with_store_mut_at(test_str, |s| {
            s.add_or_update(TrustedDevice::new("d1", "Dev 1", "tok_1", None, "inbound"));
        });
        assert!(res.is_err());

        // Original file was quarantined
        let bad_path = format!("{}.corrupt.{}", test_str, std::process::id());
        assert!(std::path::Path::new(&bad_path).exists());

        let _ = std::fs::remove_file(&bad_path);
        let _ = std::fs::remove_file(&test_path);
    }

    #[test]
    fn truncated_store_is_rejected_not_partially_loaded() {
        let test_path = std::env::temp_dir().join(format!("yb_test_trunc_store_{}.json", std::process::id()));
        let test_str = test_path.to_str().unwrap();

        let full = DeviceStore {
            devices: vec![TrustedDevice::new("d1", "Dev 1", "tok_1", Some("10.0.0.1"), "inbound")],
        }
        .to_json();
        // Cut the trailing "}\n]" — array-prefixed but truncated mid-tail.
        // The old first-character sniff accepted this; from_json would have
        // silently loaded it as a partial store.
        let truncated = &full[..full.len() - 3];
        assert!(store_json_is_complete(&full));
        assert!(!store_json_is_complete(truncated));

        std::fs::write(&test_path, truncated).unwrap();
        assert!(DeviceStore::load_result(test_str).is_err());
        let res = with_store_mut_at(test_str, |s| {
            s.add_or_update(TrustedDevice::new("d2", "Dev 2", "tok_2", None, "all"));
        });
        assert!(res.is_err(), "truncated store must abort the mutation");

        let bad_path = format!("{}.corrupt.{}", test_str, std::process::id());
        assert!(std::path::Path::new(&bad_path).exists());
        assert!(!test_path.exists() || std::fs::read_to_string(test_str).unwrap().len() < full.len());

        let _ = std::fs::remove_file(&bad_path);
        let _ = std::fs::remove_file(&test_path);
    }

    #[test]
    fn refresh_device_ip_writes_only_on_change() {
        let test_path = std::env::temp_dir().join(format!("yb_test_refresh_ip_{}.json", std::process::id()));
        let test_str = test_path.to_str().unwrap();
        with_store_mut_at(test_str, |s| {
            s.add_or_update(TrustedDevice::new("d1", "Dev 1", "tok_9", Some("10.0.0.1"), "all"));
        })
        .unwrap();

        // IP change: persisted, returns true.
        assert!(refresh_device_ip(test_str, "tok_9", "10.0.0.2", 1000).unwrap());
        let store = DeviceStore::load(test_str);
        assert_eq!(store.find_by_id("d1").unwrap().last_ip.as_deref(), Some("10.0.0.2"));
        assert_eq!(store.find_by_id("d1").unwrap().last_seen, 1000);

        // Same IP: no flash write — last_seen must NOT advance to 2000.
        assert!(!refresh_device_ip(test_str, "tok_9", "10.0.0.2", 2000).unwrap());
        let store = DeviceStore::load(test_str);
        assert_eq!(store.find_by_id("d1").unwrap().last_seen, 1000, "unchanged IP must not trigger a save");

        let _ = std::fs::remove_file(&test_path);
    }

    #[test]
    fn migrate_legacy_file_moves_old_store_once() {
        let base = std::env::temp_dir().join(format!("yb_test_migrate_{}", std::process::id()));
        let legacy = base.join("legacy.json");
        let private_dir = base.join("private");
        let private = private_dir.join("devices.json");
        let _ = std::fs::create_dir_all(&base);

        with_store_mut_at(legacy.to_str().unwrap(), |s| {
            s.add_or_update(TrustedDevice::new("d1", "Dev 1", "tok_1", Some("10.0.0.1"), "all"));
        })
        .unwrap();

        migrate_legacy_file(legacy.to_str().unwrap(), private.to_str().unwrap());
        assert!(!legacy.exists(), "legacy copy must be retired");
        assert!(private.exists());
        assert_eq!(DeviceStore::load(private.to_str().unwrap()).devices.len(), 1);

        // Idempotent: a new legacy file does not clobber the established private store.
        with_store_mut_at(legacy.to_str().unwrap(), |s| {
            s.add_or_update(TrustedDevice::new("d2", "Dev 2", "tok_2", None, "inbound"));
        })
        .unwrap();
        migrate_legacy_file(legacy.to_str().unwrap(), private.to_str().unwrap());
        assert_eq!(DeviceStore::load(private.to_str().unwrap()).devices.len(), 1);

        let _ = std::fs::remove_dir_all(&base);
    }
}

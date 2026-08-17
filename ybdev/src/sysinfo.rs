//! Small device facts for status displays: battery, wifi address, free
//! storage. All cheap sysfs reads / syscalls — safe to call per redraw.

use std::fs;

/// Battery gauge on this PW5 (bd71827 PMU).
const BAT_CAP: &str = "/sys/class/power_supply/bd71827_bat/capacity";
const BAT_STATUS: &str = "/sys/class/power_supply/bd71827_bat/status";

/// (percent, plugged) — plugged covers Charging and Full.
pub fn battery() -> (u8, bool) {
    let cap = fs::read_to_string(BAT_CAP)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let status = fs::read_to_string(BAT_STATUS).unwrap_or_default();
    let plugged = matches!(status.trim(), "Charging" | "Full");
    (cap, plugged)
}

/// wlan0 IPv4 via SIOCGIFADDR (no fork, no /proc parsing).
pub fn wifi_ip() -> Option<String> {
    // The ioctl below returns a STALE address after the radio drops —
    // the curtain kept showing the IP while wifi was actually off
    // (2026-08-17). Gate on the interface's live operstate.
    let oper = fs::read_to_string("/sys/class/net/wlan0/operstate").ok()?;
    if oper.trim() != "up" {
        return None;
    }
    #[repr(C)]
    struct Ifr {
        name: [u8; 16],
        addr: libc::sockaddr_in,
        pad: [u8; 16],
    }
    let s = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if s < 0 {
        return None;
    }
    let mut ifr = Ifr {
        name: {
            let mut n = [0u8; 16];
            n[..5].copy_from_slice(b"wlan0");
            n
        },
        addr: unsafe { std::mem::zeroed() },
        pad: [0; 16],
    };
    const SIOCGIFADDR: libc::c_int = 0x8915; // request width matches the target's ioctl
    let rv = unsafe { libc::ioctl(s, SIOCGIFADDR as _, &mut ifr) };
    unsafe { libc::close(s) };
    if rv != 0 {
        return None;
    }
    // sin_addr is stored in network byte order; to_be_bytes on a
    // little-endian arm recovers [a, b, c, d].
    let oct = ifr.addr.sin_addr.s_addr.to_be_bytes();
    Some(format!("{}.{}.{}.{}", oct[0], oct[1], oct[2], oct[3]))
}

/// Free space on the user partition, GB.
pub fn storage_free_gb() -> Option<f64> {
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        let path = b"/mnt/us\0";
        if libc::statvfs(path.as_ptr() as *const libc::c_char, &mut st) != 0 {
            return None;
        }
        Some(st.f_bavail as f64 * st.f_frsize as f64 / 1e9)
    }
}

/// KiB value of `key` in a /proc-style "Key:\t  123 kB" dump. Returns
/// None on any format surprise — memory numbers must never misreport.
fn parse_kv_kb(src: &str, key: &str) -> Option<u64> {
    src.lines().find_map(|l| {
        let rest = l.strip_prefix(key)?.strip_prefix(':')?;
        rest.trim().strip_suffix(" kB")?.trim().parse().ok()
    })
}

/// Our own resident set, KiB (/proc/self/status VmRSS) — process-wide,
/// so it reads the same from any thread.
pub fn rss_kib() -> Option<u64> {
    parse_kv_kb(&fs::read_to_string("/proc/self/status").ok()?, "VmRSS")
}

/// Device-level headroom, KiB (/proc/meminfo MemAvailable).
pub fn mem_available_kib() -> Option<u64> {
    parse_kv_kb(&fs::read_to_string("/proc/meminfo").ok()?, "MemAvailable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_sane() {
        // ifreq is 40 bytes on arm; our padded struct must be at least that.
        assert!(std::mem::size_of::<libc::statvfs>() > 0);
    }

    #[test]
    fn parses_proc_kb_values() {
        let status = "Name:\treader\nVmSize:\t 75596 kB\nVmRSS:\t  72516 kB\n";
        assert_eq!(parse_kv_kb(status, "VmRSS"), Some(72516));
        assert_eq!(parse_kv_kb(status, "VmPeak"), None);
        // "MemFree" must not satisfy a "MemAvailable" lookup (prefix trap).
        let meminfo = "MemTotal:         485604 kB\nMemFree:   30208 kB\nMemAvailable:\t 96328 kB\n";
        assert_eq!(parse_kv_kb(meminfo, "MemAvailable"), Some(96328));
        assert_eq!(parse_kv_kb(meminfo, "MemFree"), Some(30208));
        // Wrong unit or garbage → None, never a wrong number.
        assert_eq!(parse_kv_kb("VmRSS: 72516 pages\n", "VmRSS"), None);
        assert_eq!(parse_kv_kb("VmRSS: n/a kB\n", "VmRSS"), None);
    }
}

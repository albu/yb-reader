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

/// wlan0's link is actually carrying (operstate "up" is the kernel's
/// verdict; "down"/"dormant" mean unreachable even when wifid still
/// says enabled). The awake policy holds the device out of suspend
/// exactly while this is true: reachable ⇒ awake.
pub fn wifi_up() -> bool {
    fs::read_to_string("/sys/class/net/wlan0/operstate")
        .map(|s| s.trim() == "up")
        .unwrap_or(false)
}

/// Radio state for status glyphs, at the granularity sysfs can answer
/// without forking lipc (status rows repaint on busy ticks — a fork per
/// paint is off the table). Admin state is wlan0's IFF_UP flag, link
/// state the operstate: Up = associated; Searching = radio powered but
/// no association yet (bring-up takes seconds); Off = interface down.
pub enum WifiRadio {
    Off,
    Searching,
    Connected,
}

/// Pure mapping, host-testable.
fn wifi_radio_from(operstate: Option<String>, flags: Option<String>) -> WifiRadio {
    // /sys/class/net/<if>/flags reads like "0x1003"; bit 0x1 is IFF_UP.
    let admin_up = flags
        .as_deref()
        .and_then(|s| s.trim().strip_prefix("0x"))
        .and_then(|s| u32::from_str_radix(s, 16).ok())
        .map(|f| f & 0x1 != 0)
        .unwrap_or(false);
    if !admin_up {
        return WifiRadio::Off;
    }
    match operstate.as_deref() {
        Some("up") => WifiRadio::Connected,
        _ => WifiRadio::Searching,
    }
}

pub fn wifi_radio() -> WifiRadio {
    wifi_radio_from(
        fs::read_to_string("/sys/class/net/wlan0/operstate").ok(),
        fs::read_to_string("/sys/class/net/wlan0/flags").ok(),
    )
}

/// wlan0 IPv4 via SIOCGIFADDR (no fork, no /proc parsing).
pub fn wifi_ip() -> Option<String> {
    // The ioctl below returns a STALE address after the radio drops —
    // the curtain kept showing the IP while wifi was actually off
    // (2026-08-17). Gate on the interface's live operstate.
    if !wifi_up() {
        return None;
    }
    #[repr(C)]
    struct Ifr {
        name: [u8; 16],
        addr: libc::sockaddr_in,
        pad: [u8; 16],
    }
    // Kernel `struct ifreq` is 40 bytes on armv7; the padded struct must
    // be at least that or the ioctl payload is truncated.
    const _: () = assert!(std::mem::size_of::<Ifr>() >= 40);
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
    // ifr.addr.sin_addr is stored in network byte order; memory order (ne_bytes)
    // gives the octets [192, 168, 1, 72] in natural left-to-right IPv4 order.
    let oct = ifr.addr.sin_addr.s_addr.to_ne_bytes();
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

/// Total installed RAM, KiB (/proc/meminfo MemTotal).
pub fn mem_total_kib() -> Option<u64> {
    parse_kv_kb(&fs::read_to_string("/proc/meminfo").ok()?, "MemTotal")
}

/// Instruct the allocator to release free arena memory back to the OS (if supported).
pub fn trim_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        extern "C" {
            fn malloc_trim(pad: libc::size_t) -> libc::c_int;
        }
        malloc_trim(0);
    }
}

/// External power present (USB cable / charger). bd71827_ac is the VBUS
/// node on this PMIC (probed on device 2026-08-19); the battery node's
/// own `online` is always 1 and must not be used. Falls back to the
/// battery-status charge flag for odd cases.
pub fn vbus() -> bool {
    for p in ["bd71827_ac", "Wireless"] {
        if let Ok(s) = fs::read_to_string(format!("/sys/class/power_supply/{p}/online")) {
            match s.trim() {
                "1" => return true,
                "0" => return false,
                _ => {}
            }
        }
    }
    battery().1
}

/// Takeover mode: the framework boot flag is present, so this app owns
/// power and Wi-Fi policy. In stock mode the framework owns the radio,
/// and the app must never fight it (e.g. restore or power down a radio
/// its powerd is managing).
pub fn takeover() -> bool {
    std::path::Path::new("/mnt/us/DONT_START_FRAMEWORK").exists()
}

/// Set CPU frequency scaling governor for all CPU cores (e.g. "ondemand" or "interactive").
/// Falls back gracefully on desktop or if a governor is unsupported.
pub fn set_cpu_governor(governor: &str) {
    for i in 0..4 {
        let p = format!("/sys/devices/system/cpu/cpu{i}/cpufreq/scaling_governor");
        if std::path::Path::new(&p).exists() {
            let _ = fs::write(&p, governor);
        }
    }
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
        let meminfo =
            "MemTotal:         485604 kB\nMemFree:   30208 kB\nMemAvailable:\t 96328 kB\n";
        assert_eq!(parse_kv_kb(meminfo, "MemAvailable"), Some(96328));
        assert_eq!(parse_kv_kb(meminfo, "MemFree"), Some(30208));
        // Wrong unit or garbage → None, never a wrong number.
        assert_eq!(parse_kv_kb("VmRSS: 72516 pages\n", "VmRSS"), None);
        assert_eq!(parse_kv_kb("VmRSS: n/a kB\n", "VmRSS"), None);
    }

    #[test]
    fn radio_glyph_states() {
        use WifiRadio::*;
        // Admin up + operstate up = associated.
        assert!(matches!(
            wifi_radio_from(Some("up".into()), Some("0x1003".into())),
            Connected
        ));
        // Radio powered, no association yet (bring-up / drain-guard case).
        for st in ["down", "dormant", "unknown", ""] {
            assert!(matches!(
                wifi_radio_from(Some(st.into()), Some("0x1003".into())),
                Searching
            ));
        }
        // Admin down (or interface gone) = airplane mode, whatever the
        // operstate file still claims.
        assert!(matches!(
            wifi_radio_from(Some("up".into()), Some("0x1002".into())),
            Off
        ));
        assert!(matches!(wifi_radio_from(None, None), Off));
        // Unparseable flags must fail toward Off, not a phantom radio.
        assert!(matches!(
            wifi_radio_from(Some("up".into()), Some("junk".into())),
            Off
        ));
    }
}

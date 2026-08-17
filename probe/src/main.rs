//! Device introspection for the Kindle: run on-device and send the output
//! back, like yb-mirror's probe.sh but deeper (panel ioctls included).

use std::process::Command;

use ybdev::frontlight::Frontlight;
use ybdev::input;
use ybdev::panel::Panel;

fn sh(cmd: &str, args: &[&str]) -> String {
    match Command::new(cmd).args(args).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(e) => format!("<{}: {}>", cmd, e),
    }
}

fn main() {
    println!("=== system ===");
    println!("uname: {}", sh("uname", &["-a"]));
    for p in ["/etc/version", "/etc/prettyversion.txt"] {
        println!("{}: {}", p, std::fs::read_to_string(p).unwrap_or_default().trim());
    }

    println!("\n=== panel ===");
    match Panel::open() {
        Ok(p) => {
            println!(
                "fb0: {}x{} stride={} bpp={}",
                p.width, p.height, p.stride, p.bpp
            );
            println!("first 8 fb bytes: {:02x?}", &p.buf()[..8.min(p.buf().len())]);
        }
        Err(e) => println!("panel: {}", e),
    }
    for f in ybdev::panel::sysfs_paths() {
        println!("{}: {}", f, std::fs::read_to_string(f).unwrap_or_default().trim());
    }

    println!("\n=== input ===");
    println!("discovered touch: {:?}", input::discover());
    if let Some(path) = input::discover() {
        println!("abs ranges for {}", path);
        if let Ok(f) = std::fs::File::open(&path) {
            use std::os::unix::io::AsRawFd;
            #[repr(C)]
            #[derive(Default)]
            struct InputAbsinfo {
                value: i32,
                min: i32,
                max: i32,
                fuzz: i32,
                flat: i32,
                resolution: i32,
            }
            const fn eviocgabs(nr: u32) -> ybdev::mtk::Ioctl {
                let v = ((2u64) << 30) | ((24u64) << 16) | ((b'E' as u64) << 8) | (0x40 + nr as u64);
                v as ybdev::mtk::Ioctl
            }
            for (name, nr) in [
                ("ABS_X", 0x00u32),
                ("ABS_Y", 0x01),
                ("ABS_MT_POSITION_X", 0x35),
                ("ABS_MT_POSITION_Y", 0x36),
                ("ABS_MT_TRACKING_ID", 0x39),
            ] {
                let mut a = InputAbsinfo::default();
                let rv = unsafe { libc::ioctl(f.as_raw_fd(), eviocgabs(nr), &mut a) };
                if rv == 0 {
                    println!(
                        "  {}: value={} min={} max={} fuzz={} flat={}",
                        name, a.value, a.min, a.max, a.fuzz, a.flat
                    );
                }
            }
        }
    }
    println!(
        "/proc/bus/input/devices:\n{}",
        std::fs::read_to_string("/proc/bus/input/devices").unwrap_or_default()
    );

    println!("\n=== frontlight ===");
    match Frontlight::open() {
        Ok(fl) => println!(
            "max={} current={} amber1_max={} amber2_max={}",
            fl.max(),
            fl.get(),
            fl.amber1_max(),
            fl.amber2_max()
        ),
        Err(e) => println!("frontlight: {}", e),
    }

    println!("\n=== storage ===");
    println!("koreader git-rev: {}", std::fs::read_to_string("/mnt/us/koreader/git-rev").unwrap_or_default().trim());
    println!("documents:");
    if let Ok(rd) = std::fs::read_dir("/mnt/us/documents") {
        for e in rd.flatten().take(20) {
            println!("  {}", e.file_name().to_string_lossy());
        }
    }
}

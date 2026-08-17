//! Kindle frontlight via /dev/frontlight ioctls (PW5 kernel header,
//! <linux/frontlight.h>, magic 'L').

use std::fs::File;
use std::os::unix::io::AsRawFd;

use crate::mtk::Ioctl;

const FL_MAGIC: u8 = b'L';
const DEV: &str = "/dev/frontlight";

const fn fl_ioctl(nr: u32, write: bool) -> Ioctl {
    let dir = if write { 1u32 } else { 2u32 };
    let v = ((dir as u64) << 30)
        | ((4u64) << 16)
        | ((FL_MAGIC as u64) << 8)
        | (nr as u64);
    v as Ioctl
}

const FL_IOCTL_SET_INTENSITY: Ioctl = fl_ioctl(0x01, true);
const FL_IOCTL_GET_INTENSITY: Ioctl = fl_ioctl(0x02, false);
const FL_IOCTL_GET_RANGE_MAX: Ioctl = fl_ioctl(0x03, false);
const FL_IOCTL_SET_INTENSITY_AMBER_1: Ioctl = fl_ioctl(0x05, true);
const FL_IOCTL_GET_INTENSITY_AMBER_1: Ioctl = fl_ioctl(0x06, false);
const FL_IOCTL_GET_RANGE_MAX_AMBER_1: Ioctl = fl_ioctl(0x07, false);
const FL_IOCTL_SET_INTENSITY_AMBER_2: Ioctl = fl_ioctl(0x09, true);
const FL_IOCTL_GET_INTENSITY_AMBER_2: Ioctl = fl_ioctl(0x0a, false);
const FL_IOCTL_GET_RANGE_MAX_AMBER_2: Ioctl = fl_ioctl(0x0b, false);

pub struct Frontlight {
    f: File,
}

// The warm channel on the PW5 (FW 5.19): the /dev/frontlight amber ioctls
// report max 0, but the FP9966 driver exposes both LED channels as standard
// backlight devices. Per KOReader's device table the mapping is
// fp9966-bl1 = white, fp9966-bl0 = amber/tone (bl0-first reads natural,
// which is how the channels got swapped here once already).
const BL0_BRIGHTNESS: &str = "/sys/class/backlight/fp9966-bl0/brightness";
const BL0_MAX: &str = "/sys/class/backlight/fp9966-bl0/max_brightness";

fn read_i32(path: &str) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

impl Frontlight {
    pub fn open() -> Result<Frontlight, String> {
        let f = File::options()
            .read(true)
            .write(true)
            .open(DEV)
            .map_err(|e| format!("open {}: {}", DEV, e))?;
        Ok(Frontlight { f })
    }

    fn ioctl_val(&self, req: Ioctl) -> std::io::Result<i32> {
        let mut v: i32 = 0;
        let rv = unsafe { libc::ioctl(self.f.as_raw_fd(), req, &mut v) };
        if rv < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(v)
        }
    }

    fn ioctl_set(&self, req: Ioctl, v: i32) -> std::io::Result<()> {
        let rv = unsafe { libc::ioctl(self.f.as_raw_fd(), req, &v) };
        if rv < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn max(&self) -> i32 {
        self.ioctl_val(FL_IOCTL_GET_RANGE_MAX).unwrap_or(24)
    }
    pub fn get(&self) -> i32 {
        self.ioctl_val(FL_IOCTL_GET_INTENSITY).unwrap_or(0)
    }
    pub fn set(&self, v: i32) {
        let _ = self.ioctl_set(FL_IOCTL_SET_INTENSITY, v);
    }

    pub fn amber1_max(&self) -> i32 {
        self.ioctl_val(FL_IOCTL_GET_RANGE_MAX_AMBER_1).unwrap_or(0)
    }
    pub fn amber1_get(&self) -> i32 {
        self.ioctl_val(FL_IOCTL_GET_INTENSITY_AMBER_1).unwrap_or(0)
    }
    pub fn amber1_set(&self, v: i32) {
        let _ = self.ioctl_set(FL_IOCTL_SET_INTENSITY_AMBER_1, v);
    }

    pub fn amber2_max(&self) -> i32 {
        self.ioctl_val(FL_IOCTL_GET_RANGE_MAX_AMBER_2).unwrap_or(0)
    }
    pub fn amber2_get(&self) -> i32 {
        self.ioctl_val(FL_IOCTL_GET_INTENSITY_AMBER_2).unwrap_or(0)
    }
    pub fn amber2_set(&self, v: i32) {
        let _ = self.ioctl_set(FL_IOCTL_SET_INTENSITY_AMBER_2, v);
    }

    /// Warm/amber channel via sysfs (the ioctls don't serve it on FW 5.19).
    pub fn tone_max(&self) -> i32 {
        read_i32(BL0_MAX).unwrap_or(0)
    }
    pub fn tone_get(&self) -> i32 {
        read_i32(BL0_BRIGHTNESS).unwrap_or(0)
    }
    pub fn tone_set(&self, v: i32) {
        let _ = std::fs::write(BL0_BRIGHTNESS, format!("{}\n", v));
    }
}

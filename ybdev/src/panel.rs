//! E-ink framebuffer access for MTK Kindles: geometry from
//! FBIOGET_VSCREENINFO (the *real* panel size), direct mmap writes, and
//! MXCFB_SEND_UPDATE_MTK refreshes.
//!
//! Why not sysfs virtual_size? On the PW5 it reports 1248x3296: the width
//! is padded to the line_length (1248 = stride for a 1236 px panel) and the
//! height is doubled (2x1648, the MTK driver's virtual buffer). The real
//! panel is 1236x1648 at the top of the fb; the ioctl exposes xres/yres.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use crate::mtk;

const FB_PATH: &str = "/dev/fb0";
const FBIOGET_VSCREENINFO: crate::mtk::Ioctl = 0x4600;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FbBitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FbVarScreeninfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: FbBitfield,
    green: FbBitfield,
    blue: FbBitfield,
    transp: FbBitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    pixclock: u32,
    left_margin: u32,
    right_margin: u32,
    upper_margin: u32,
    lower_margin: u32,
    hsync_len: u32,
    vsync_len: u32,
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}

const _: () = assert!(std::mem::size_of::<FbVarScreeninfo>() == 160);

pub struct Panel {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bpp: u32,
    fb: File,
    map: *mut u8,
    map_len: usize,
    marker: u32,
}

// SAFETY(Send): the mmap'ed framebuffer is kernel-owned memory with no
// thread affinity, and Panel is deliberately NOT `Sync` — Rust's
// ownership model then enforces the real invariant: after a move, only
// the receiving thread holds it, so concurrent access through borrows
// cannot be expressed. Residual hazards are outside the type system:
// a second independently-opened Panel aliases the same pages (keep it
// single-instance), and the display controller writes MAP_SHARED pages
// concurrently by design — standard framebuffer practice.
unsafe impl Send for Panel {}

fn read_sysfs_line(path: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.trim().to_string())
}

fn sysfs_u32(path: &str) -> Option<u32> {
    read_sysfs_line(path)?.parse().ok()
}

pub fn sysfs_paths() -> [&'static str; 3] {
    [
        "/sys/class/graphics/fb0/virtual_size",
        "/sys/class/graphics/fb0/stride",
        "/sys/class/graphics/fb0/bits_per_pixel",
    ]
}

impl Panel {
    pub fn open() -> Result<Panel, String> {
        let fb = OpenOptions::new()
            .read(true)
            .write(true)
            .open(FB_PATH)
            .map_err(|e| format!("open {}: {}", FB_PATH, e))?;

        // The ioctl gives the real panel size (xres/yres) plus the virtual
        // buffer (xres_virtual/yres_virtual) the kernel allocated. The
        // drawable area is the first yres rows; the extra virtual height on
        // MTK (2x) is managed by the driver, we just map it all.
        let mut vinfo = FbVarScreeninfo::default();
        let rv = unsafe { libc::ioctl(fb.as_raw_fd(), FBIOGET_VSCREENINFO, &mut vinfo) };
        let (width, height, virtual_height) = if rv == 0 && vinfo.xres != 0 && vinfo.yres != 0 {
            (vinfo.xres, vinfo.yres, vinfo.yres_virtual.max(vinfo.yres))
        } else {
            // Fallback: sysfs virtual_size (padded/doubled on MTK; the
            // ioctl should work on the Kindle, this is just a safety net).
            let mut w = 1236u32;
            let mut h = 1648u32;
            if let Some(vs) = read_sysfs_line("/sys/class/graphics/fb0/virtual_size") {
                let mut it = vs.split([',', ' ']).filter(|s| !s.is_empty());
                if let (Some(w0), Some(h0)) = (it.next(), it.next()) {
                    if let (Ok(w0), Ok(h0)) = (w0.parse::<u32>(), h0.parse::<u32>()) {
                        w = w0;
                        h = h0;
                    }
                }
            }
            // Try to undo the MTK padding/doubling: the panel is square-ish
            // portrait; if height is exactly 2x and width has padding, use
            // the PW5 logical size as the drawable area.
            if h.is_multiple_of(2) && w >= 1236 {
                (1236, h / 2, h)
            } else {
                (w, h, h)
            }
        };
        let stride = sysfs_u32("/sys/class/graphics/fb0/stride").unwrap_or(width);
        let bpp = sysfs_u32("/sys/class/graphics/fb0/bits_per_pixel").unwrap_or(8);

        let map_len_full = stride as usize * virtual_height as usize;
        let (map, map_len) = unsafe {
            let m = libc::mmap(
                std::ptr::null_mut(),
                map_len_full,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fb.as_raw_fd(),
                0,
            );
            if m == libc::MAP_FAILED {
                // Some kernels refuse to map the full virtual size; the
                // drawable area only needs stride * yres.
                let map_len = stride as usize * height as usize;
                let m = libc::mmap(
                    std::ptr::null_mut(),
                    map_len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fb.as_raw_fd(),
                    0,
                );
                if m == libc::MAP_FAILED {
                    return Err(format!("mmap {}: {}", FB_PATH, io::Error::last_os_error()));
                }
                (m as *mut u8, map_len)
            } else {
                (m as *mut u8, map_len_full)
            }
        };

        Ok(Panel {
            width,
            height,
            stride,
            bpp,
            fb,
            map,
            map_len,
            marker: 0,
        })
    }

    /// Read-only view of the whole framebuffer. The slice's lifetime is
    /// tied to `&self`, so it cannot overlap a [`Panel::buf_mut`] borrow
    /// through this Panel — the raw pointer inside does not weaken that;
    /// only re-deriving pointers from the slice would.
    pub fn buf(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.map, self.map_len) }
    }

    /// Size of the mmap'ed region (stride * mapped rows).
    pub fn map_len(&self) -> usize {
        self.map_len
    }

    /// Mutable view of the whole framebuffer. Exclusive against other
    /// Panel borrows by signature; the underlying pages are MAP_SHARED,
    /// so the display controller may write them concurrently — that is
    /// inherent to framebuffers, not an aliasing bug.
    pub fn buf_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.map, self.map_len) }
    }

    /// Copy `data` (w*h grayscale bytes) into the framebuffer at (x, y).
    pub fn blit(&mut self, x: u32, y: u32, w: u32, h: u32, data: &[u8]) {
        // usize math throughout: `w * h` in u32 wraps on a >4k×>1M or
        // padded buffer, under-checking `data.len()` and slicing past the
        // source below.
        let (w, h) = (w as usize, h as usize);
        if data.len() < w * h {
            return;
        }
        let stride = self.stride as usize;
        for row in 0..h {
            let sy = (y as usize) + row;
            if sy >= self.height as usize {
                break;
            }
            let sx = x as usize;
            if sx + w > stride {
                continue;
            }
            let dst = sy * stride + sx;
            let src = row * w;
            self.buf_mut()[dst..dst + w].copy_from_slice(&data[src..src + w]);
        }
    }

    pub fn fill(&mut self, v: u8) {
        let len = self
            .map_len
            .min(self.stride as usize * self.height as usize);
        self.buf_mut()[..len].fill(v);
    }

    /// Fence: wait for the previous update's submission before sending a
    /// new one. Two unfenced updates back-to-back race on the EPDC and the
    /// panel shows interleaved/stale content (visible as jumping text) —
    /// this is KOReader's discipline on Kindles, verbatim.
    fn fence(&mut self) {
        if self.marker != 0 {
            let _ = mtk::wait_update_submission(self.fb.as_raw_fd(), self.marker);
        }
    }

    /// Partial refresh (no flash), the default for page turns. GL16, not
    /// AUTO: AUTO's histogram hints resolve text-like regions to DU —
    /// the ~2-level mode — which snaps the renderer's anti-aliasing ramp
    /// to black/white and reads as blurry text on glass (2026-08-21:
    /// source framebuffer verified crisp, ~4.6% AA-gray pixels). GL16
    /// reproduces all 16 levels without flashing, at ~1.5–2× DU's
    /// update time — the trade a text reader wants.
    pub fn refresh_partial(&mut self, x: u32, y: u32, w: u32, h: u32) {
        self.fence();
        self.marker = self.marker.wrapping_add(1);
        let region = mtk::MxcfbRect {
            top: y,
            left: x,
            width: w,
            height: h,
        };
        let _ = mtk::send_update(
            self.fb.as_raw_fd(),
            region,
            mtk::WAVEFORM_GL16,
            mtk::UPDATE_MODE_PARTIAL,
            self.marker,
        );
    }

    /// Fast A2 refresh for animation frames: 2 gray levels only, quick
    /// update time, and it ghosts — the caller must end the animation
    /// with a full (GC16) refresh to clean the panel. A2 snaps every
    /// anti-aliased pixel to black/white, which is exactly right for
    /// chunky line art and wrong for text.
    pub fn refresh_fast(&mut self, x: u32, y: u32, w: u32, h: u32) {
        self.fence();
        self.marker = self.marker.wrapping_add(1);
        let region = mtk::MxcfbRect {
            top: y,
            left: x,
            width: w,
            height: h,
        };
        let _ = mtk::send_update(
            self.fb.as_raw_fd(),
            region,
            mtk::WAVEFORM_A2,
            mtk::UPDATE_MODE_PARTIAL,
            self.marker,
        );
    }

    /// Full (flashing) refresh, to clear ghosting.
    pub fn refresh_full(&mut self) {
        self.fence();
        self.marker = self.marker.wrapping_add(1);
        let region = mtk::MxcfbRect {
            top: 0,
            left: 0,
            width: self.width,
            height: self.height,
        };
        let _ = mtk::send_update(
            self.fb.as_raw_fd(),
            region,
            mtk::WAVEFORM_GC16,
            mtk::UPDATE_MODE_FULL,
            self.marker,
        );
    }

    /// Convenience: blit a full-screen buffer and refresh.
    pub fn present(&mut self, data: &[u8], full: bool) {
        self.blit(0, 0, self.width, self.height, data);
        if full {
            self.refresh_full();
        } else {
            self.refresh_partial(0, 0, self.width, self.height);
        }
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        if !self.map.is_null() {
            unsafe {
                libc::munmap(self.map as *mut libc::c_void, self.map_len);
            }
            self.map = std::ptr::null_mut();
        }
    }
}

pub fn fb_path_exists() -> bool {
    Path::new(FB_PATH).exists()
}

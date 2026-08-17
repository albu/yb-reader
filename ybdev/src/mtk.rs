//! MediaTek e-ink ("hwtcon") panel API, ported from FBInk's
//! eink/mtk-kindle.h (last updated from the PW5/PW6 kernels).
//! The MTK driver's API is the mxcfb API with extra fields.

use std::mem::size_of;

// libc::ioctl's request argument type: c_ulong on glibc/macOS, c_int on musl.
#[cfg(all(unix, target_env = "musl"))]
pub type Ioctl = libc::c_int;
#[cfg(all(unix, not(target_env = "musl")))]
pub type Ioctl = libc::c_ulong;

// --- ioctl request encoding (asm-generic) ---
const IOC_READ: u32 = 2;
const IOC_WRITE: u32 = 1;

const fn ioc(dir: u32, typ: u8, nr: u32, size: usize) -> Ioctl {
    let v = ((dir as u64) << 30) | ((size as u64) << 16) | ((typ as u64) << 8) | (nr as u64);
    v as Ioctl
}

const fn iow(typ: u8, nr: u32, size: usize) -> Ioctl {
    ioc(IOC_WRITE, typ, nr, size)
}
const fn ior(typ: u8, nr: u32, size: usize) -> Ioctl {
    ioc(IOC_READ, typ, nr, size)
}
const fn iowr(typ: u8, nr: u32, size: usize) -> Ioctl {
    ioc(IOC_READ | IOC_WRITE, typ, nr, size)
}

// --- structs (repr(C), mirroring the kernel headers) ---

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MxcfbRect {
    pub top: u32,
    pub left: u32,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MxcfbAltBufferData {
    pub phys_addr: u32,
    pub width: u32,
    pub height: u32,
    pub alt_update_region: MxcfbRect,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MxcfbSwipeData {
    pub direction: u32,
    pub steps: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MxcfbUpdateDataMtk {
    pub update_region: MxcfbRect,
    pub waveform_mode: u32,
    pub update_mode: u32,
    pub update_marker: u32,
    pub temp: i32,
    pub flags: u32,
    pub dither_mode: i32,
    pub quant_bit: i32,
    pub alt_buffer_data: MxcfbAltBufferData,
    pub swipe_data: MxcfbSwipeData,
    pub hist_bw_waveform_mode: u32,
    pub hist_gray_waveform_mode: u32,
    pub ts_pxp: u32,
    pub ts_epdc: u32,
}

// Sanity: the ioctl numbers encode sizeof(), so the layout must be exact.
const _: () = assert!(size_of::<MxcfbRect>() == 16);
const _: () = assert!(size_of::<MxcfbAltBufferData>() == 28);
const _: () = assert!(size_of::<MxcfbSwipeData>() == 8);
const _: () = assert!(size_of::<MxcfbUpdateDataMtk>() == 96);
const _: () = assert!(std::mem::align_of::<MxcfbUpdateDataMtk>() == 4);

// --- ioctls (HWTCON magic 'F') ---
pub const MXCFB_SEND_UPDATE_MTK: Ioctl =
    iow(b'F', 0x2E, size_of::<MxcfbUpdateDataMtk>());
pub const MXCFB_WAIT_FOR_ANY_UPDATE_COMPLETE_MTK: Ioctl =
    iowr(b'F', 0x37, size_of::<u32>());
// Kindle's MXCFB_WAIT_FOR_UPDATE_SUBMISSION == 0x40044637 (_IOW 'F' 0x37 u32).
// KOReader waits on this before every flashing/UI refresh on Kindles
// ("Kindles wait for submission of the previous marker") — the fence that
// keeps two back-to-back updates from racing on the EPDC.
pub const MXCFB_WAIT_FOR_UPDATE_SUBMISSION: Ioctl =
    iow(b'F', 0x37, size_of::<u32>());
pub const MXCFB_SET_UPDATE_SCHEME: Ioctl = iow(b'F', 0x32, size_of::<u32>());
pub const MXCFB_SET_PWRDOWN_DELAY: Ioctl = iow(b'F', 0x30, size_of::<i32>());
pub const MXCFB_GET_TEMPERATURE: Ioctl = ior(b'F', 0x38, size_of::<i32>());

// --- waveform modes (MTK_WAVEFORM_MODE_ENUM) ---
pub const WAVEFORM_INIT: u32 = 0;
pub const WAVEFORM_DU: u32 = 1;
pub const WAVEFORM_GC16: u32 = 2;
pub const WAVEFORM_GL16: u32 = 3;
pub const WAVEFORM_GLR16: u32 = 4;
pub const WAVEFORM_GLD16: u32 = 5;
pub const WAVEFORM_A2: u32 = 6;
pub const WAVEFORM_DU4: u32 = 7;
pub const WAVEFORM_GC16_PARTIAL: u32 = 10;
pub const WAVEFORM_AUTO: u32 = 257;

// --- update modes ---
pub const UPDATE_MODE_PARTIAL: u32 = 0;
pub const UPDATE_MODE_FULL: u32 = 1;

// --- temp ---
pub const TEMP_USE_AMBIENT: i32 = 0x1000;

// --- flags (the ones we care about) ---
pub const MTK_EPDC_FLAG_SKIP_CFA: u32 = 0x10;
pub const MTK_EPDC_FLAG_USE_DITHERING_Y4: u32 = 0x4000;
pub const MTK_EPDC_FLAG_ENABLE_SWIPE: u32 = 0x10000;

// --- dither modes ---
pub const DITHER_PASSTHROUGH: i32 = 0;

/// Send a panel update; the exact recipe from FBInk's refresh_kindle_mtk.
pub fn send_update(
    fd: i32,
    region: MxcfbRect,
    waveform: u32,
    update_mode: u32,
    marker: u32,
) -> std::io::Result<()> {
    let mut update = MxcfbUpdateDataMtk {
        update_region: region,
        waveform_mode: waveform,
        update_mode,
        update_marker: marker,
        temp: TEMP_USE_AMBIENT,
        flags: 0,
        dither_mode: DITHER_PASSTHROUGH,
        quant_bit: 0,
        alt_buffer_data: MxcfbAltBufferData::default(),
        swipe_data: MxcfbSwipeData::default(),
        hist_bw_waveform_mode: WAVEFORM_DU,
        hist_gray_waveform_mode: WAVEFORM_GC16,
        ts_pxp: 0,
        ts_epdc: 0,
    };
    let rv = unsafe { libc::ioctl(fd, MXCFB_SEND_UPDATE_MTK, &mut update) };
    if rv < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Wait until update `marker` has been submitted to the EPDC. Sent before
/// the next update so a fresh one can never overtake an in-flight one.
pub fn wait_update_submission(fd: i32, marker: u32) -> std::io::Result<()> {
    let mut m = marker;
    let rv = unsafe { libc::ioctl(fd, MXCFB_WAIT_FOR_UPDATE_SUBMISSION, &mut m) };
    if rv < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

//! V4L2 subdev ioctls: media-bus formats and sensor controls.
//!
//! The ov7251's `analogue_gain`/`exposure` must be set *and read back* around
//! every capture — `analogue_gain` reverts to its minimum of 16 and yields a
//! near-black frame that mimics a dark room (garden `hardware/notes/005`,
//! the 2026-08-21 trap). The readback lives in the daemon; this module is the
//! set/get surface.

use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;

/// `VIDIOC_SUBDEV_G_FMT` / `VIDIOC_SUBDEV_S_FMT` — media-bus format on a pad.
pub const VIDIOC_SUBDEV_G_FMT: libc::c_ulong = 0xc0585604;
pub const VIDIOC_SUBDEV_S_FMT: libc::c_ulong = 0xc0585605;

/// Plain `VIDIOC_G_CTRL` / `VIDIOC_S_CTRL` also work on a subdev fd (the
/// subdev ioctl layer multiplexes them); the deprecated `VIDIOC_SUBDEV_*_CTRL`
/// aliases were dropped from the UAPI header on this kernel.
pub const VIDIOC_G_CTRL: libc::c_ulong = 0xc008561b;
pub const VIDIOC_S_CTRL: libc::c_ulong = 0xc008561c;

/// `V4L2_SUBDEV_FORMAT_ACTIVE` — set the live format, not a try format.
pub const SUBDEV_FORMAT_ACTIVE: u32 = 1;

/// `V4L2_CID_ANALOGUE_GAIN` (ov7251) — dlib scored best at 128; the auth
/// path's default.
pub const CID_ANALOGUE_GAIN: u32 = 0x9e0903;
/// `V4L2_CID_EXPOSURE` — the auth path's default (ir-up.sh's 1704).
pub const CID_EXPOSURE: u32 = 0x980911;

/// `struct v4l2_mbus_framefmt` — 48 bytes on this kernel.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MbusFramefmt {
    pub width: u32,          // 0
    pub height: u32,         // 4
    pub code: u32,           // 8
    pub field: u32,          // 12
    pub colorspace: u32,     // 16
    pub ycbcr_enc: u16,      // 20
    pub quantization: u16,   // 22
    pub xfer_func: u16,      // 24
    pub flags: u16,          // 26
    pub reserved: [u16; 10], // 28..48
}

/// `struct v4l2_subdev_format` — 88 bytes on this kernel.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SubdevFormat {
    pub which: u32,       // 0
    pub pad: u32,         // 4
    pub format: MbusFramefmt, // 8..56
    pub stream: u32,      // 56
    pub reserved: [u32; 7], // 60..88
}

/// `struct v4l2_control` — 8 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Control {
    pub id: u32,
    pub value: i32,
}

/// Set the media-bus format on one pad of a subdev — the
/// `media-ctl -V '"ov7251 3-0060":0 [fmt:Y10_1X10/640x480]'` equivalent.
pub fn set_format(fd: &File, pad: u32, code: u32, width: u32, height: u32) -> io::Result<()> {
    let mut fmt = SubdevFormat {
        which: SUBDEV_FORMAT_ACTIVE,
        pad,
        format: MbusFramefmt {
            width,
            height,
            code,
            field: 0,
            colorspace: 0,
            ycbcr_enc: 0,
            quantization: 0,
            xfer_func: 0,
            flags: 0,
            reserved: [0; 10],
        },
        stream: 0,
        reserved: [0; 7],
    };
    let r = unsafe {
        libc::ioctl(fd.as_raw_fd(), VIDIOC_SUBDEV_S_FMT, &mut fmt as *mut SubdevFormat as usize)
    };
    if r == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Set one sensor control (`v4l2-ctl --set-ctrl` on the subdev node).
pub fn set_ctrl(fd: &File, id: u32, value: i32) -> io::Result<()> {
    let mut c = Control { id, value };
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), VIDIOC_S_CTRL, &mut c as *mut Control as usize) };
    if r == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Read one sensor control back (`v4l2-ctl --get-ctrl`).
pub fn get_ctrl(fd: &File, id: u32) -> io::Result<i32> {
    let mut c = Control { id, value: 0 };
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), VIDIOC_G_CTRL, &mut c as *mut Control as usize) };
    if r == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(c.value)
}

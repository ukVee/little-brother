//! V4L2 video-node ioctls and the mmap streaming loops.
//!
//! `Stream` owns a device's buffer ring the way the old pipeline never could:
//! it is **one process** doing STREAMON → DQBUF/process/QBUF → STREAMOFF, so
//! "never shoot a capture mid-DQBUF" (the lesson that makes STREAMON return
//! EINVAL and the sensor mute on I2C) is structural — there is no subprocess
//! to kill. `stop()` runs the clean STREAMOFF; `Drop` is the safety net that
//! still tries (SIGKILL of the daemon leaves the ~13 s hardware watchdog and
//! the sensor's own power-off as the last lines, same as today).
//!
//! **Multiplanar handling** (learned the hard way during slice 006 bring-up):
//! the CIO2 node is a multiplanar-only device — it *reports* `Video Capture
//! Multiplanar` and `S_FMT`/`REQBUFS` with the single-planar type fail EINVAL,
//! while `v4l2-ctl` silently switches to the mplane type via
//! `VIDIOC_QUERYCAP`. `Stream::start` does the same detection: query the
//! device caps, pick single-planar or mplane type, use the matching
//! `pix`/`pix_mp` payload and the per-plane buffer arrays.

use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;

use crate::fourcc;

pub const VIDIOC_QUERYCAP: libc::c_ulong = 0x80685600;
pub const VIDIOC_G_FMT: libc::c_ulong = 0xc0d05604;
pub const VIDIOC_S_FMT: libc::c_ulong = 0xc0d05605;
pub const VIDIOC_REQBUFS: libc::c_ulong = 0xc0145608;
pub const VIDIOC_QUERYBUF: libc::c_ulong = 0xc0585609;
pub const VIDIOC_QBUF: libc::c_ulong = 0xc058560f;
pub const VIDIOC_DQBUF: libc::c_ulong = 0xc0585611;
pub const VIDIOC_STREAMON: libc::c_ulong = 0x40045612;
pub const VIDIOC_STREAMOFF: libc::c_ulong = 0x40045613;

pub const BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
pub const BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
pub const BUF_TYPE_VIDEO_CAPTURE_MPLANE: u32 = 9;
pub const BUF_TYPE_VIDEO_OUTPUT_MPLANE: u32 = 10;
pub const MEMORY_MMAP: u32 = 1;
pub const FIELD_NONE: u32 = 1;
pub const FIELD_ANY: u32 = 0;
pub const COLORSPACE_RAW: u32 = 0xb;

pub const CAP_VIDEO_CAPTURE: u32 = 0x1;
pub const CAP_VIDEO_OUTPUT: u32 = 0x2;
pub const CAP_VIDEO_CAPTURE_MPLANE: u32 = 0x1000;
pub const CAP_VIDEO_OUTPUT_MPLANE: u32 = 0x2000;
pub const CAP_STREAMING: u32 = 0x0400_0000;
pub const CAP_DEVICE_CAPS: u32 = 0x8000_0000;


/// `struct v4l2_capability` — 104 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Capability {
    pub driver: [u8; 16],
    pub card: [u8; 32],
    pub bus_info: [u8; 32],
    pub version: u32,
    pub capabilities: u32,
    pub device_caps: u32,
    pub reserved: [u32; 3],
}

/// `struct v4l2_format` — 208 bytes on this kernel: `type` + a 200-byte union
/// (offset 8, aligned for the u64-bearing members).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Format {
    pub ty: u32,
    pub _pad: u32,
    pub raw: [u8; 200],
}

/// `struct v4l2_pix_format` — 48 bytes (single-planar).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PixFormat {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub field: u32,
    pub bytesperline: u32,
    pub sizeimage: u32,
    pub colorspace: u32,
    pub priv_: u32,
    pub flags: u32,
    pub ycbcr_enc: u32,
    pub quantization: u32,
    pub xfer_func: u32,
}

/// `struct v4l2_plane_pix_format` — 20 bytes (packed in the kernel header).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PlanePixFormat {
    pub sizeimage: u32,
    pub bytesperline: u32,
    pub reserved: [u16; 6],
}

/// `struct v4l2_pix_format_mplane` — 192 bytes (packed in the kernel header;
/// the natural repr(C) layout matches: no padding at all).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PixFormatMplane {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub field: u32,
    pub colorspace: u32,
    pub plane_fmt: [PlanePixFormat; 8],
    pub num_planes: u8,
    pub flags: u8,
    pub ycbcr_enc: u8,
    pub quantization: u8,
    pub xfer_func: u8,
    pub reserved: [u8; 7],
}

/// `struct v4l2_requestbuffers` — 20 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RequestBuffers {
    pub count: u32,
    pub ty: u32,
    pub memory: u32,
    pub capabilities: u32,
    pub flags: u8,
    pub reserved: [u8; 3],
}

/// `struct v4l2_plane` — 64 bytes; the per-plane buffer info for mplane.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Plane {
    pub bytesused: u32,
    pub length: u32,
    pub m: u64, // union: mem_offset u32 / userptr usize / fd i32
    pub data_offset: u32,
    pub reserved: [u32; 11],
}

/// `struct v4l2_buffer` — 88 bytes on this kernel (timestamp is `timeval`,
/// timecode is 16 bytes; `m` union is 8 bytes at offset 64).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Buffer {
    pub index: u32,        // 0
    pub ty: u32,           // 4
    pub bytesused: u32,    // 8
    pub flags: u32,        // 12
    pub field: u32,        // 16
    pub timestamp: [i64; 2], // 20..36  (struct timeval)
    pub timecode: [u32; 4],  // 36..52  (struct v4l2_timecode)
    pub sequence: u32,     // 52
    pub memory: u32,       // 56
    pub m: u64,            // 64..72  (offset u32 / userptr usize / planes ptr / fd)
    pub length: u32,       // 72
    pub reserved2: u32,    // 76
    pub request_fd: i32,   // 80
    pub _pad: u32,         // 84
}

fn zeroed_buffer() -> Buffer {
    Buffer {
        index: 0,
        ty: 0,
        bytesused: 0,
        flags: 0,
        field: FIELD_NONE,
        timestamp: [0; 2],
        timecode: [0; 4],
        sequence: 0,
        memory: 0,
        m: 0,
        length: 0,
        reserved2: 0,
        request_fd: -1,
        _pad: 0,
    }
}

fn ioctl(fd: &File, request: libc::c_ulong, arg: usize) -> io::Result<()> {
    match unsafe { libc::ioctl(fd.as_raw_fd(), request, arg) } {
        -1 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

fn open(dev: &str) -> io::Result<File> {
    // Always O_RDWR: mmap(PROT_WRITE) is refused on an O_RDONLY fd, and both
    // the capture and the loopback sink stream through mmap (v4l2-ctl opens
    // O_RDWR for exactly this reason).
    std::fs::OpenOptions::new().read(true).write(true).open(dev)
}

/// The capture format the IR path streams: ip3y 640x480.
pub fn ip3y_640x480() -> PixFormat {
    PixFormat {
        width: 640,
        height: 480,
        pixelformat: fourcc::IPU3_Y10,
        field: FIELD_ANY,
        bytesperline: 0,
        sizeimage: 0, // driver fills; ip3y is 399 360 B/frame
        colorspace: COLORSPACE_RAW,
        ..PixFormat::default()
    }
}

/// The loopback sink format: 8-bit gray 640x480, 307 200 B/frame.
pub fn grey_640x480() -> PixFormat {
    PixFormat {
        width: 640,
        height: 480,
        pixelformat: fourcc::GREY,
        field: FIELD_NONE,
        bytesperline: 640,
        sizeimage: 640 * 480,
        colorspace: 0,
        ..PixFormat::default()
    }
}

/// Query the device's capabilities and pick the buffer type that matches —
/// single-planar or multiplanar (the CIO2 is mplane-only; v4l2loopback is
/// classic single-planar). `wanted` is the capture/output class.
fn pick_type(fd: &File, capture: bool) -> io::Result<(u32, bool)> {
    let mut cap = Capability {
        driver: [0; 16],
        card: [0; 32],
        bus_info: [0; 32],
        version: 0,
        capabilities: 0,
        device_caps: 0,
        reserved: [0; 3],
    };
    ioctl(fd, VIDIOC_QUERYCAP, &mut cap as *mut Capability as usize)?;
    let caps = if cap.capabilities & CAP_DEVICE_CAPS != 0 {
        cap.device_caps
    } else {
        cap.capabilities
    };
    if caps & CAP_STREAMING == 0 {
        return Err(io::Error::other("device does not support streaming I/O"));
    }
    let (single, multi) = if capture {
        (CAP_VIDEO_CAPTURE, CAP_VIDEO_CAPTURE_MPLANE)
    } else {
        (CAP_VIDEO_OUTPUT, CAP_VIDEO_OUTPUT_MPLANE)
    };
    if caps & multi != 0 {
        Ok((if capture { BUF_TYPE_VIDEO_CAPTURE_MPLANE } else { BUF_TYPE_VIDEO_OUTPUT_MPLANE }, true))
    } else if caps & single != 0 {
        Ok((if capture { BUF_TYPE_VIDEO_CAPTURE } else { BUF_TYPE_VIDEO_OUTPUT }, false))
    } else {
        Err(io::Error::other(if capture { "device has no video capture capability" } else { "device has no video output capability" }))
    }
}

fn is_mplane(ty: u32) -> bool {
    ty == BUF_TYPE_VIDEO_CAPTURE_MPLANE || ty == BUF_TYPE_VIDEO_OUTPUT_MPLANE
}

fn is_output(ty: u32) -> bool {
    ty == BUF_TYPE_VIDEO_OUTPUT || ty == BUF_TYPE_VIDEO_OUTPUT_MPLANE
}

fn write_pix(dst: &mut [u8; 200], pix: &PixFormat) {
    let mut i = 0;
    for v in [
        pix.width,
        pix.height,
        pix.pixelformat,
        pix.field,
        pix.bytesperline,
        pix.sizeimage,
        pix.colorspace,
        pix.priv_,
        pix.flags,
        pix.ycbcr_enc,
        pix.quantization,
        pix.xfer_func,
    ] {
        dst[i..i + 4].copy_from_slice(&v.to_le_bytes());
        i += 4;
    }
}

fn write_pix_mp(dst: &mut [u8; 200], pix: &PixFormat) {
    let u32s = [pix.width, pix.height, pix.pixelformat, pix.field, pix.colorspace];
    for (i, v) in u32s.iter().enumerate() {
        dst[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    // plane_fmt[0]: sizeimage = pix.sizeimage, bytesperline = pix.bytesperline
    dst[20..24].copy_from_slice(&pix.sizeimage.to_le_bytes());
    dst[24..28].copy_from_slice(&pix.bytesperline.to_le_bytes());
    // num_planes at offset 180 (20 + 8*20)
    dst[180] = 1;
}

/// Set the pixel format, single-planar or mplane depending on `ty`.
pub fn set_pix_format(fd: &File, ty: u32, pix: &PixFormat) -> io::Result<()> {
    let mut f = Format { ty, _pad: 0, raw: [0; 200] };
    if is_mplane(ty) {
        write_pix_mp(&mut f.raw, pix);
    } else {
        write_pix(&mut f.raw, pix);
    }
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), VIDIOC_S_FMT, &mut f as *mut Format as usize) };
    if r == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

struct MapBuf {
    ptr: *mut libc::c_void,
    len: usize,
}

impl MapBuf {
    // SAFETY: ptr/len come from one mmap call; the caller guarantees the
    // mapping outlives the slice.
    unsafe fn as_slice(&self) -> &[u8] {
        std::slice::from_raw_parts(self.ptr as *const u8, self.len)
    }
    unsafe fn as_slice_mut(&mut self) -> &mut [u8] {
        std::slice::from_raw_parts_mut(self.ptr as *mut u8, self.len)
    }
}

impl Drop for MapBuf {
    fn drop(&mut self) {
        // SAFETY: an mmap this struct owns, unmapped exactly once.
        unsafe { libc::munmap(self.ptr, self.len) };
    }
}

// SAFETY: the daemon is single-threaded, but Stream may be moved across
// threads by future callers; the mapping is a plain kernel buffer.
unsafe impl Send for MapBuf {}

/// One mmap'd streaming device: capture (CIO2, mplane) or output
/// (v4l2loopback, single-planar).
pub struct Stream {
    fd: File,
    bufs: Vec<MapBuf>,
    ty: u32,
    /// The buffer most recently DQBUF'd and not yet Q'd back, if any.
    dq: Option<u32>,
}

impl Stream {
    /// Request `nbufs` mmap buffers, map them, queue them, and STREAMON.
    /// Detects single-planar vs mplane from VIDIOC_QUERYCAP like v4l2-ctl.
    pub fn start(dev: &str, capture: bool, pix: &PixFormat, nbufs: u32) -> io::Result<Stream> {
        let fd = open(dev)?;
        let (ty, mplane) = pick_type(&fd, capture)?;
        if let Err(e) = set_pix_format(&fd, ty, pix) {
            return Err(io::Error::new(
                e.kind(),
                format!("{dev}: cannot set format: {e}"),
            ));
        }
        let mut req = RequestBuffers {
            count: nbufs,
            ty,
            memory: MEMORY_MMAP,
            capabilities: 0,
            flags: 0,
            reserved: [0; 3],
        };
        ioctl(&fd, VIDIOC_REQBUFS, &mut req as *mut RequestBuffers as usize)?;
        if req.count < 2 {
            return Err(io::Error::other(format!(
                "{dev}: driver granted {} buffers, need >= 2",
                req.count
            )));
        }

        let mut bufs = Vec::with_capacity(req.count as usize);
        for i in 0..req.count {
            let mut plane = Plane { bytesused: 0, length: 0, m: 0, data_offset: 0, reserved: [0; 11] };
            let mut b = Buffer {
                index: i,
                ty,
                memory: MEMORY_MMAP,
                ..zeroed_buffer()
            };
            if mplane {
                b.m = &mut plane as *mut Plane as u64;
                b.length = 1;
            }
            ioctl(&fd, VIDIOC_QUERYBUF, &mut b as *mut Buffer as usize)?;
            let (offset, len) = if mplane {
                (plane.m as u32, plane.length as usize)
            } else {
                (b.m as u32, b.length as usize)
            };
            // SAFETY: mapping a kernel video buffer at its QUERYBUF offset.
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd.as_raw_fd(),
                    offset as libc::off_t,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            bufs.push(MapBuf { ptr, len });
        }

        // Queue all buffers before STREAMON.
        for i in 0..req.count {
            let mut plane = Plane { bytesused: 0, length: 0, m: 0, data_offset: 0, reserved: [0; 11] };
            let mut b = Buffer {
                index: i,
                ty,
                memory: MEMORY_MMAP,
                ..zeroed_buffer()
            };
            if mplane {
                b.m = &mut plane as *mut Plane as u64;
                b.length = 1;
            }
            ioctl(&fd, VIDIOC_QBUF, &mut b as *mut Buffer as usize)?;
        }
        ioctl(&fd, VIDIOC_STREAMON, &ty as *const u32 as usize)?;
        Ok(Stream { fd, bufs, ty, dq: None })
    }

    fn poll_ready(&self, timeout_ms: i32) -> io::Result<bool> {
        let event = if is_output(self.ty) { libc::POLLOUT } else { libc::POLLIN };
        let mut pfd = libc::pollfd { fd: self.fd.as_raw_fd(), events: event, revents: 0 };
        // SAFETY: pfd is a single caller-owned pollfd; timeout_ms >= 0.
        let r = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if r == -1 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(e);
        }
        Ok(r > 0 && pfd.revents & event != 0)
    }

    /// DQBUF (blocking until a frame, bounded by the poll timeout), handing
    /// the frame to `f`. Returns `Ok(None)` on timeout (no frame this tick),
    /// `Ok(Some(t))` with `f`'s result, or the first ioctl/poll error.
    ///
    /// The frame slice borrows the kernel buffer: it must not outlive `f`.
    /// The buffer is Q'd back before `f` returns, so the ring keeps turning.
    pub fn with_frame<T>(
        &mut self,
        timeout_ms: i32,
        f: impl FnOnce(&[u8]) -> io::Result<T>,
    ) -> io::Result<Option<T>> {
        // Refuse a nested call: exactly one buffer may be outstanding.
        if self.dq.is_some() {
            return Err(io::Error::other(
                "with_frame called with a buffer already dequeued",
            ));
        }
        if !self.poll_ready(timeout_ms)? {
            return Ok(None);
        }
        let mplane = is_mplane(self.ty);
        let mut plane = Plane { bytesused: 0, length: 0, m: 0, data_offset: 0, reserved: [0; 11] };
        let mut b = Buffer { ty: self.ty, memory: MEMORY_MMAP, ..zeroed_buffer() };
        if mplane {
            b.m = &mut plane as *mut Plane as u64;
            b.length = 1;
        }
        ioctl(&self.fd, VIDIOC_DQBUF, &mut b as *mut Buffer as usize)?;
        self.dq = Some(b.index);
        let used = if mplane { plane.bytesused as usize } else { b.bytesused as usize };
        let t = {
            // SAFETY: b.index was granted by REQBUFS; the mapping lives in
            // self.bufs and aliases nothing (one DQ at a time).
            let slice = unsafe { self.bufs[b.index as usize].as_slice() };
            f(&slice[..used.min(slice.len())])
        }?;
        let mut qb = Buffer { index: b.index, ty: self.ty, memory: MEMORY_MMAP, ..zeroed_buffer() };
        if mplane {
            plane = Plane { bytesused: 0, length: 0, m: 0, data_offset: 0, reserved: [0; 11] };
            qb.m = &mut plane as *mut Plane as u64;
            qb.length = 1;
        }
        ioctl(&self.fd, VIDIOC_QBUF, &mut qb as *mut Buffer as usize)?;
        self.dq = None;
        Ok(Some(t))
    }

    /// Write one frame into the output device (v4l2loopback sink): DQBUF a
    /// free buffer, copy, QBUF it filled. `Ok(false)` = no buffer free within
    /// the timeout (the reader is not consuming — drop the frame, like
    /// ffmpeg's pacing did).
    pub fn write_frame(&mut self, frame: &[u8], timeout_ms: i32) -> io::Result<bool> {
        debug_assert!(is_output(self.ty));
        if self.dq.is_some() {
            return Err(io::Error::other(
                "write_frame called with a buffer already dequeued",
            ));
        }
        if !self.poll_ready(timeout_ms)? {
            return Ok(false);
        }
        let mplane = is_mplane(self.ty);
        let mut plane = Plane { bytesused: 0, length: 0, m: 0, data_offset: 0, reserved: [0; 11] };
        let mut b = Buffer { ty: self.ty, memory: MEMORY_MMAP, ..zeroed_buffer() };
        if mplane {
            b.m = &mut plane as *mut Plane as u64;
            b.length = 1;
        }
        ioctl(&self.fd, VIDIOC_DQBUF, &mut b as *mut Buffer as usize)?;
        self.dq = Some(b.index);
        {
            // SAFETY: b.index was granted by REQBUFS and is currently ours;
            // as_slice_mut aliases nothing (single-threaded, one DQ at a time).
            let dst = unsafe { self.bufs[b.index as usize].as_slice_mut() };
            if dst.len() < frame.len() {
                return Err(io::Error::other(format!(
                    "sink buffer is {} bytes, frame is {}",
                    dst.len(),
                    frame.len()
                )));
            }
            dst[..frame.len()].copy_from_slice(frame);
        }
        let mut qb = Buffer { index: b.index, ty: self.ty, memory: MEMORY_MMAP, ..zeroed_buffer() };
        if mplane {
            plane = Plane {
                bytesused: frame.len() as u32,
                length: 0,
                m: 0,
                data_offset: 0,
                reserved: [0; 11],
            };
            qb.m = &mut plane as *mut Plane as u64;
            qb.length = 1;
        } else {
            qb.bytesused = frame.len() as u32;
        }
        ioctl(&self.fd, VIDIOC_QBUF, &mut qb as *mut Buffer as usize)?;
        self.dq = None;
        Ok(true)
    }

    /// The clean STREAMOFF + REQBUFS(0); the wedge-free teardown this whole
    /// slice exists to own. Never kill a subprocess mid-DQBUF — there is none.
    pub fn stop(&mut self) -> io::Result<()> {
        if self.ty == 0 {
            return Ok(());
        }
        let ty = self.ty;
        let r = ioctl(&self.fd, VIDIOC_STREAMOFF, &ty as *const u32 as usize);
        self.ty = 0;
        // Free the buffer ring (REQBUFS count 0); munmap happens on Drop.
        let mut req = RequestBuffers {
            count: 0,
            ty,
            memory: MEMORY_MMAP,
            capabilities: 0,
            flags: 0,
            reserved: [0; 3],
        };
        let _ = ioctl(&self.fd, VIDIOC_REQBUFS, &mut req as *mut RequestBuffers as usize);
        r
    }

    /// The buffer type this stream runs (0 after stop).
    pub fn ty(&self) -> u32 {
        self.ty
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // Safety net: STREAMOFF + free on every path the daemon can die on.
        // Errors here cannot be reported — stop() is the reporting path.
        if self.ty != 0 {
            let ty = self.ty;
            let _ = ioctl(&self.fd, VIDIOC_STREAMOFF, &ty as *const u32 as usize);
        }
        // MapBuf::drop munmaps each mapping.
    }
}

// SAFETY: nothing in the libc call path borrows anything thread-local.
unsafe impl Send for Stream {}

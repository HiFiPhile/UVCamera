// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! Minimal V4L2 output device bindings for the f_uvc gadget video node.
//!
//! Struct layouts mirror `linux/videodev2.h` and `linux/usb/g_uvc.h`. Sizes
//! are part of the ioctl numbers, so the definitions use C types that match
//! the target ABI (pointer-sized unions, `timeval`, `timespec`).

use std::{
    ffi::{CStr, c_int, c_ulong, c_void},
    fs::OpenOptions,
    io,
    mem::size_of,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
        unix::fs::OpenOptionsExt,
    },
    path::Path,
    ptr::NonNull,
};

use anyhow::{Context, Result, bail};

use crate::uvc::CtrlRequest;

// ioctl number encoding (asm-generic, also used by arm, arm64, x86).
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> c_ulong {
    ((dir << 30) | ((size as u32) << 16) | ((ty as u32) << 8) | nr as u32) as c_ulong
}

const fn ior<T>(ty: u8, nr: u8) -> c_ulong {
    ioc(IOC_READ, ty, nr, size_of::<T>())
}

const fn iow<T>(ty: u8, nr: u8) -> c_ulong {
    ioc(IOC_WRITE, ty, nr, size_of::<T>())
}

const fn iowr<T>(ty: u8, nr: u8) -> c_ulong {
    ioc(IOC_READ | IOC_WRITE, ty, nr, size_of::<T>())
}

const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const V4L2_MEMORY_MMAP: u32 = 1;
const V4L2_FIELD_NONE: u32 = 1;
const V4L2_BUF_FLAG_ERROR: u32 = 0x40;
const V4L2_EVENT_PRIVATE_START: u32 = 0x0800_0000;

pub const UVC_EVENT_CONNECT: u32 = V4L2_EVENT_PRIVATE_START;
pub const UVC_EVENT_DISCONNECT: u32 = V4L2_EVENT_PRIVATE_START + 1;
pub const UVC_EVENT_STREAMON: u32 = V4L2_EVENT_PRIVATE_START + 2;
pub const UVC_EVENT_STREAMOFF: u32 = V4L2_EVENT_PRIVATE_START + 3;
pub const UVC_EVENT_SETUP: u32 = V4L2_EVENT_PRIVATE_START + 4;
pub const UVC_EVENT_DATA: u32 = V4L2_EVENT_PRIVATE_START + 5;

#[repr(C)]
struct Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

#[repr(C)]
union FormatUnion {
    pix: PixFormat,
    raw_data: [u8; 200],
    // struct v4l2_window contains pointers, making the union pointer-aligned.
    _align: *mut c_void,
}

#[repr(C)]
struct Format {
    type_: u32,
    fmt: FormatUnion,
}

#[repr(C)]
struct OutputParm {
    capability: u32,
    outputmode: u32,
    // struct v4l2_fract timeperframe
    numerator: u32,
    denominator: u32,
    extendedmode: u32,
    writebuffers: u32,
    reserved: [u32; 4],
}

#[repr(C)]
struct StreamParm {
    type_: u32,
    output: OutputParm,
    // Rest of the 200 byte union.
    _pad: [u8; 160],
}

#[repr(C)]
struct RequestBuffers {
    count: u32,
    type_: u32,
    memory: u32,
    capabilities: u32,
    flags: u8,
    reserved: [u8; 3],
}

#[repr(C)]
struct Timecode {
    type_: u32,
    flags: u32,
    frames: u8,
    seconds: u8,
    minutes: u8,
    hours: u8,
    userbits: [u8; 4],
}

#[repr(C)]
union BufferM {
    offset: u32,
    userptr: c_ulong,
    planes: *mut c_void,
    fd: i32,
}

#[repr(C)]
struct Buffer {
    index: u32,
    type_: u32,
    bytesused: u32,
    flags: u32,
    field: u32,
    timestamp: libc::timeval,
    timecode: Timecode,
    sequence: u32,
    memory: u32,
    m: BufferM,
    length: u32,
    reserved2: u32,
    request_fd: i32,
}

#[repr(C)]
struct EventSubscription {
    type_: u32,
    id: u32,
    flags: u32,
    reserved: [u32; 5],
}

#[repr(C)]
union EventData {
    data: [u8; 64],
    // Several event payloads contain __s64, making the union 8-byte aligned.
    _align: [u64; 8],
}

#[repr(C)]
struct RawEvent {
    type_: u32,
    u: EventData,
    pending: u32,
    sequence: u32,
    timestamp: libc::timespec,
    id: u32,
    reserved: [u32; 8],
}

#[repr(C)]
struct UvcRequestData {
    length: i32,
    data: [u8; 60],
}

// Sizes from the C headers (clang/NDK, both ABIs we ship), since a layout
// mistake would silently produce a different ioctl number.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(
    size_of::<Format>() == 208 && size_of::<Buffer>() == 88 && size_of::<RawEvent>() == 136
);
#[cfg(all(target_pointer_width = "32", target_arch = "arm"))]
const _: () = assert!(
    size_of::<Format>() == 204 && size_of::<Buffer>() == 68 && size_of::<RawEvent>() == 128
);
const _: () = assert!(
    size_of::<Capability>() == 104
        && size_of::<StreamParm>() == 204
        && size_of::<RequestBuffers>() == 20
        && size_of::<EventSubscription>() == 32
        && size_of::<UvcRequestData>() == 64
);

const VIDIOC_QUERYCAP: c_ulong = ior::<Capability>(b'V', 0);
const VIDIOC_S_FMT: c_ulong = iowr::<Format>(b'V', 5);
const VIDIOC_REQBUFS: c_ulong = iowr::<RequestBuffers>(b'V', 8);
const VIDIOC_QUERYBUF: c_ulong = iowr::<Buffer>(b'V', 9);
const VIDIOC_QBUF: c_ulong = iowr::<Buffer>(b'V', 15);
const VIDIOC_DQBUF: c_ulong = iowr::<Buffer>(b'V', 17);
const VIDIOC_S_PARM: c_ulong = iowr::<StreamParm>(b'V', 22);
const VIDIOC_STREAMON: c_ulong = iow::<c_int>(b'V', 18);
const VIDIOC_STREAMOFF: c_ulong = iow::<c_int>(b'V', 19);
const VIDIOC_DQEVENT: c_ulong = ior::<RawEvent>(b'V', 89);
const VIDIOC_SUBSCRIBE_EVENT: c_ulong = iow::<EventSubscription>(b'V', 90);
const UVCIOC_SEND_RESPONSE: c_ulong = iow::<UvcRequestData>(b'U', 1);

/// Maximum payload of a UVC control response.
pub const MAX_RESPONSE_LEN: usize = 60;

/// Issue an ioctl, retrying on EINTR.
///
/// # Safety
///
/// `arg` must point to a value of the type `request` expects.
unsafe fn ioctl<T>(fd: BorrowedFd, request: c_ulong, arg: *mut T) -> io::Result<()> {
    loop {
        // The request parameter type differs between bionic (c_int) and glibc
        // (c_ulong), hence the inferred cast.
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg) };
        if ret >= 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// `enum usb_device_speed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Speed {
    Unknown,
    Low,
    Full,
    High,
    Wireless,
    Super,
    SuperPlus,
}

impl From<u32> for Speed {
    fn from(value: u32) -> Self {
        match value {
            1 => Self::Low,
            2 => Self::Full,
            3 => Self::High,
            4 => Self::Wireless,
            5 => Self::Super,
            6 => Self::SuperPlus,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Connect(Speed),
    Disconnect,
    StreamOn,
    StreamOff,
    Setup(CtrlRequest),
    Data(Vec<u8>),
    Other(u32),
}

impl Event {
    fn from_raw(raw: &RawEvent) -> Self {
        // SAFETY: Every bit pattern is a valid [u8; 64].
        let data = unsafe { &raw.u.data };
        match raw.type_ {
            UVC_EVENT_CONNECT => {
                Self::Connect(u32::from_ne_bytes(data[..4].try_into().unwrap()).into())
            }
            UVC_EVENT_DISCONNECT => Self::Disconnect,
            UVC_EVENT_STREAMON => Self::StreamOn,
            UVC_EVENT_STREAMOFF => Self::StreamOff,
            UVC_EVENT_SETUP => Self::Setup(CtrlRequest::from_bytes(data[..8].try_into().unwrap())),
            UVC_EVENT_DATA => {
                let len = i32::from_ne_bytes(data[..4].try_into().unwrap());
                let len = (len.max(0) as usize).min(MAX_RESPONSE_LEN);
                Self::Data(data[4..4 + len].to_vec())
            }
            t => Self::Other(t),
        }
    }
}

struct MappedBuffer {
    ptr: NonNull<u8>,
    len: usize,
}

impl Drop for MappedBuffer {
    fn drop(&mut self) {
        // SAFETY: ptr/len came from a successful mmap.
        unsafe {
            libc::munmap(self.ptr.as_ptr().cast(), self.len);
        }
    }
}

/// An opened f_uvc video node. Closing it deactivates the USB function,
/// which disconnects the whole gadget from the host.
pub struct VideoDevice {
    fd: OwnedFd,
    buffers: Vec<MappedBuffer>,
}

impl VideoDevice {
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .with_context(|| format!("Failed to open video device: {path:?}"))?;

        Ok(Self {
            fd: file.into(),
            buffers: vec![],
        })
    }

    /// Returns `(driver, card)`.
    pub fn query_capability(&self) -> Result<(String, String)> {
        // SAFETY: Plain-old-data struct.
        let mut cap: Capability = unsafe { std::mem::zeroed() };
        unsafe { ioctl(self.fd.as_fd(), VIDIOC_QUERYCAP, &mut cap) }.context("VIDIOC_QUERYCAP")?;

        let s = |b: &[u8]| {
            CStr::from_bytes_until_nul(b)
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        Ok((s(&cap.driver), s(&cap.card)))
    }

    /// Subscribe to all UVC events. `UVC_EVENT_SETUP` must come last: it is
    /// what activates the USB function and lets the gadget connect.
    pub fn subscribe_uvc_events(&self) -> Result<()> {
        for type_ in [
            UVC_EVENT_CONNECT,
            UVC_EVENT_DISCONNECT,
            UVC_EVENT_STREAMON,
            UVC_EVENT_STREAMOFF,
            UVC_EVENT_DATA,
            UVC_EVENT_SETUP,
        ] {
            let mut sub = EventSubscription {
                type_,
                id: 0,
                flags: 0,
                reserved: [0; 5],
            };
            unsafe { ioctl(self.fd.as_fd(), VIDIOC_SUBSCRIBE_EVENT, &mut sub) }
                .with_context(|| format!("VIDIOC_SUBSCRIBE_EVENT {type_:#x}"))?;
        }
        Ok(())
    }

    /// Returns `None` when no event is pending.
    pub fn dequeue_event(&self) -> Result<Option<Event>> {
        // SAFETY: Plain-old-data struct.
        let mut raw: RawEvent = unsafe { std::mem::zeroed() };
        match unsafe { ioctl(self.fd.as_fd(), VIDIOC_DQEVENT, &mut raw) } {
            Ok(()) => Ok(Some(Event::from_raw(&raw))),
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(None),
            Err(e) => Err(e).context("VIDIOC_DQEVENT"),
        }
    }

    /// Complete the data stage of a control request. For host-to-device
    /// requests, pass an empty `data` with `len` = wLength to receive data.
    pub fn send_response(&self, data: &[u8], len: usize) -> Result<()> {
        let mut resp = UvcRequestData {
            length: len.min(MAX_RESPONSE_LEN) as i32,
            data: [0; 60],
        };
        let n = data.len().min(MAX_RESPONSE_LEN);
        resp.data[..n].copy_from_slice(&data[..n]);
        unsafe { ioctl(self.fd.as_fd(), UVCIOC_SEND_RESPONSE, &mut resp) }
            .context("UVCIOC_SEND_RESPONSE")
    }

    /// Stall EP0 to reject a control request.
    pub fn send_stall(&self) -> Result<()> {
        let mut resp = UvcRequestData {
            length: -libc::EL2HLT,
            data: [0; 60],
        };
        unsafe { ioctl(self.fd.as_fd(), UVCIOC_SEND_RESPONSE, &mut resp) }
            .context("UVCIOC_SEND_RESPONSE (stall)")
    }

    pub fn set_format(&self, fourcc: u32, width: u16, height: u16, sizeimage: u32) -> Result<()> {
        // SAFETY: Plain-old-data struct.
        let mut format: Format = unsafe { std::mem::zeroed() };
        format.type_ = V4L2_BUF_TYPE_VIDEO_OUTPUT;
        format.fmt.pix = PixFormat {
            width: width.into(),
            height: height.into(),
            pixelformat: fourcc,
            field: V4L2_FIELD_NONE,
            sizeimage,
            // SAFETY: Plain-old-data struct.
            ..unsafe { std::mem::zeroed() }
        };
        unsafe { ioctl(self.fd.as_fd(), VIDIOC_S_FMT, &mut format) }.context("VIDIOC_S_FMT")
    }

    /// Tell the driver the frame interval (100 ns units) so it can spread
    /// each frame evenly over the isochronous service intervals. Returns
    /// `false` on kernels without `VIDIOC_S_PARM` support (before 6.13).
    pub fn set_frame_interval(&self, interval: u32) -> Result<bool> {
        // SAFETY: Plain-old-data struct.
        let mut parm: StreamParm = unsafe { std::mem::zeroed() };
        parm.type_ = V4L2_BUF_TYPE_VIDEO_OUTPUT;
        parm.output.numerator = interval;
        parm.output.denominator = 10_000_000;
        match unsafe { ioctl(self.fd.as_fd(), VIDIOC_S_PARM, &mut parm) } {
            Ok(()) => Ok(true),
            Err(e) if e.raw_os_error() == Some(libc::ENOTTY) => Ok(false),
            Err(e) => Err(e).context("VIDIOC_S_PARM"),
        }
    }

    fn request_buffers_raw(&self, count: u32) -> Result<u32> {
        let mut req = RequestBuffers {
            count,
            type_: V4L2_BUF_TYPE_VIDEO_OUTPUT,
            memory: V4L2_MEMORY_MMAP,
            capabilities: 0,
            flags: 0,
            reserved: [0; 3],
        };
        unsafe { ioctl(self.fd.as_fd(), VIDIOC_REQBUFS, &mut req) }.context("VIDIOC_REQBUFS")?;
        Ok(req.count)
    }

    fn new_buffer(index: u32) -> Buffer {
        // SAFETY: Plain-old-data struct.
        let mut buf: Buffer = unsafe { std::mem::zeroed() };
        buf.index = index;
        buf.type_ = V4L2_BUF_TYPE_VIDEO_OUTPUT;
        buf.memory = V4L2_MEMORY_MMAP;
        buf
    }

    /// Allocate and map `count` buffers. Returns the number allocated.
    pub fn allocate_buffers(&mut self, count: u32) -> Result<usize> {
        self.free_buffers()?;

        let count = self.request_buffers_raw(count)?;
        if count == 0 {
            bail!("Driver allocated no buffers");
        }

        for index in 0..count {
            let mut buf = Self::new_buffer(index);
            unsafe { ioctl(self.fd.as_fd(), VIDIOC_QUERYBUF, &mut buf) }
                .with_context(|| format!("VIDIOC_QUERYBUF #{index}"))?;

            let len = buf.length as usize;
            // SAFETY: m.offset is the active member for MMAP buffers.
            let offset = unsafe { buf.m.offset };
            // SAFETY: Mapping a region the driver just described.
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    self.fd.as_raw_fd(),
                    offset as libc::off_t,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(io::Error::last_os_error())
                    .with_context(|| format!("Failed to mmap buffer #{index}"));
            }

            self.buffers.push(MappedBuffer {
                ptr: NonNull::new(ptr.cast()).unwrap(),
                len,
            });
        }

        Ok(self.buffers.len())
    }

    pub fn free_buffers(&mut self) -> Result<()> {
        if self.buffers.is_empty() {
            return Ok(());
        }
        self.buffers.clear();
        self.request_buffers_raw(0)?;
        Ok(())
    }

    pub fn buffer_mut(&mut self, index: usize) -> &mut [u8] {
        let buf = &self.buffers[index];
        // SAFETY: The mapping is valid for len bytes until munmap in Drop,
        // and the buffer is not queued to the driver while we write to it.
        unsafe { std::slice::from_raw_parts_mut(buf.ptr.as_ptr(), buf.len) }
    }

    pub fn queue_buffer(&self, index: usize, bytesused: usize) -> Result<()> {
        let mut buf = Self::new_buffer(index as u32);
        buf.bytesused = bytesused as u32;
        buf.field = V4L2_FIELD_NONE;
        unsafe { ioctl(self.fd.as_fd(), VIDIOC_QBUF, &mut buf) }
            .with_context(|| format!("VIDIOC_QBUF #{index}"))
    }

    /// Returns the index of a completed buffer and whether the driver
    /// flagged it as failed (e.g. part of the frame was not transferred), or
    /// `None` when no buffer has completed yet.
    pub fn dequeue_buffer(&self) -> Result<Option<(usize, bool)>> {
        let mut buf = Self::new_buffer(0);
        match unsafe { ioctl(self.fd.as_fd(), VIDIOC_DQBUF, &mut buf) } {
            Ok(()) => Ok(Some((
                buf.index as usize,
                buf.flags & V4L2_BUF_FLAG_ERROR != 0,
            ))),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e).context("VIDIOC_DQBUF"),
        }
    }

    pub fn stream_on(&self) -> Result<()> {
        let mut type_ = V4L2_BUF_TYPE_VIDEO_OUTPUT as c_int;
        unsafe { ioctl(self.fd.as_fd(), VIDIOC_STREAMON, &mut type_) }.context("VIDIOC_STREAMON")
    }

    pub fn stream_off(&self) -> Result<()> {
        let mut type_ = V4L2_BUF_TYPE_VIDEO_OUTPUT as c_int;
        unsafe { ioctl(self.fd.as_fd(), VIDIOC_STREAMOFF, &mut type_) }.context("VIDIOC_STREAMOFF")
    }
}

impl AsFd for VideoDevice {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values from the C headers, for the 64-bit ABI this test runs on.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn ioctl_numbers_match_kernel_headers() {
        assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
        assert_eq!(VIDIOC_S_FMT, 0xc0d0_5605);
        assert_eq!(VIDIOC_REQBUFS, 0xc014_5608);
        assert_eq!(VIDIOC_QUERYBUF, 0xc058_5609);
        assert_eq!(VIDIOC_QBUF, 0xc058_560f);
        assert_eq!(VIDIOC_DQBUF, 0xc058_5611);
        assert_eq!(VIDIOC_S_PARM, 0xc0cc_5616);
        assert_eq!(VIDIOC_STREAMON, 0x4004_5612);
        assert_eq!(VIDIOC_STREAMOFF, 0x4004_5613);
        assert_eq!(VIDIOC_DQEVENT, 0x8088_5659);
        assert_eq!(VIDIOC_SUBSCRIBE_EVENT, 0x4020_565a);
        assert_eq!(UVCIOC_SEND_RESPONSE, 0x4040_5501);
    }
}

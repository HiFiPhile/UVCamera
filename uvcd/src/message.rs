// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! Daemon protocol, version 1. Mirrored by the app's `daemon/Message.kt`.
//!
//! After connecting, the client sends the protocol version (u8) and the
//! daemon answers 1 (accepted) or 0. Then the client sends requests and
//! reads one response each. All integers are little endian; strings are a
//! u16 length followed by UTF-8. Invalid input closes the connection.
//!
//! ```text
//! Request  := 1 GetStatus
//!           | 2 Enable  maxburst:u8 count:u8 FormatSpec*
//!           | 3 Disable
//! FormatSpec := kind:u8 count:u8 (width:u16 height:u16 count:u8 interval:u32*)*
//! Response := 0 Error   message:string
//!           | 1 Status  enabled:u8 speed:u8 streaming:u8 [StreamParams]
//!                       fps_milli:u32 failed:u32
//!           | 2 Ok
//! StreamParams := kind:u8 width:u16 height:u16 interval:u32 max_frame_size:u32
//! ```
//!
//! `kind` is 0 for MJPEG and 1 for YUYV. `speed` is the kernel's
//! `enum usb_device_speed`, or 0 when no host has configured the function.

use std::io::{self, Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use crate::{
    stream::StreamStatus,
    uvc::{
        StreamParams,
        format::{FormatKind, FormatSpec, FrameSpec},
    },
    v4l2::Speed,
};

pub const PROTOCOL_VERSION: u8 = 1;
/// Abstract Unix socket name.
pub const SOCKET_NAME: &str = "uvcamerad";

const MAX_FORMATS: usize = 2;
const MAX_FRAMES: usize = 32;
const MAX_INTERVALS: usize = 16;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn read_string(r: &mut impl Read) -> io::Result<String> {
    let len = r.read_u16::<LittleEndian>()?;
    let mut buf = vec![0u8; len.into()];
    r.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|_| invalid("String is not UTF-8"))
}

fn write_string(w: &mut impl Write, s: &str) -> io::Result<()> {
    let len = u16::try_from(s.len()).map_err(|_| invalid("String too long"))?;
    w.write_u16::<LittleEndian>(len)?;
    w.write_all(s.as_bytes())
}

fn read_bool(r: &mut impl Read) -> io::Result<bool> {
    match r.read_u8()? {
        0 => Ok(false),
        1 => Ok(true),
        b => Err(invalid(format!("Invalid boolean: {b}"))),
    }
}

fn read_kind(r: &mut impl Read) -> io::Result<FormatKind> {
    match r.read_u8()? {
        0 => Ok(FormatKind::Mjpeg),
        1 => Ok(FormatKind::Yuyv),
        k => Err(invalid(format!("Invalid format kind: {k}"))),
    }
}

fn write_kind(w: &mut impl Write, kind: FormatKind) -> io::Result<()> {
    w.write_u8(match kind {
        FormatKind::Mjpeg => 0,
        FormatKind::Yuyv => 1,
    })
}

fn read_count(r: &mut impl Read, max: usize, what: &str) -> io::Result<usize> {
    let n = usize::from(r.read_u8()?);
    if n == 0 || n > max {
        return Err(invalid(format!("Invalid {what} count: {n} (1-{max})")));
    }
    Ok(n)
}

fn write_count(w: &mut impl Write, n: usize) -> io::Result<()> {
    w.write_u8(u8::try_from(n).map_err(|_| invalid("Too many items"))?)
}

fn read_format_spec(r: &mut impl Read) -> io::Result<FormatSpec> {
    let kind = read_kind(r)?;
    let mut frames: Vec<FrameSpec> = vec![];

    for _ in 0..read_count(r, MAX_FRAMES, "frame")? {
        let width = r.read_u16::<LittleEndian>()?;
        let height = r.read_u16::<LittleEndian>()?;
        // YUYV packs two pixels per macropixel, and both encoders need even
        // dimensions.
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            return Err(invalid(format!("Invalid frame size: {width}x{height}")));
        }
        if frames
            .iter()
            .any(|f| f.width == width && f.height == height)
        {
            return Err(invalid(format!("Duplicate frame size: {width}x{height}")));
        }

        let mut intervals = vec![];
        for _ in 0..read_count(r, MAX_INTERVALS, "interval")? {
            let interval = r.read_u32::<LittleEndian>()?;
            if interval == 0 {
                return Err(invalid("Zero frame interval"));
            }
            intervals.push(interval);
        }
        intervals.sort_unstable();
        intervals.dedup();

        frames.push(FrameSpec {
            width,
            height,
            intervals,
        });
    }

    Ok(FormatSpec { kind, frames })
}

fn write_format_spec(w: &mut impl Write, spec: &FormatSpec) -> io::Result<()> {
    write_kind(w, spec.kind)?;
    write_count(w, spec.frames.len())?;
    for frame in &spec.frames {
        w.write_u16::<LittleEndian>(frame.width)?;
        w.write_u16::<LittleEndian>(frame.height)?;
        write_count(w, frame.intervals.len())?;
        for &interval in &frame.intervals {
            w.write_u32::<LittleEndian>(interval)?;
        }
    }
    Ok(())
}

fn speed_to_u8(speed: Option<Speed>) -> u8 {
    match speed {
        None | Some(Speed::Unknown) => 0,
        Some(Speed::Low) => 1,
        Some(Speed::Full) => 2,
        Some(Speed::High) => 3,
        Some(Speed::Wireless) => 4,
        Some(Speed::Super) => 5,
        Some(Speed::SuperPlus) => 6,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnableRequest {
    pub maxburst: u8,
    pub formats: Vec<FormatSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    GetStatus,
    Enable(EnableRequest),
    Disable,
}

impl Request {
    pub fn read_from(r: &mut impl Read) -> io::Result<Self> {
        match r.read_u8()? {
            1 => Ok(Self::GetStatus),
            2 => {
                let maxburst = r.read_u8()?;
                if maxburst > 15 {
                    return Err(invalid(format!("Invalid maxburst: {maxburst}")));
                }
                let mut formats: Vec<FormatSpec> = vec![];
                for _ in 0..read_count(r, MAX_FORMATS, "format")? {
                    let spec = read_format_spec(r)?;
                    // configfs has one instance per format kind.
                    if formats.iter().any(|f| f.kind == spec.kind) {
                        return Err(invalid(format!("Duplicate format: {}", spec.kind)));
                    }
                    formats.push(spec);
                }
                Ok(Self::Enable(EnableRequest { maxburst, formats }))
            }
            3 => Ok(Self::Disable),
            id => Err(invalid(format!("Unknown request: {id}"))),
        }
    }

    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        match self {
            Self::GetStatus => w.write_u8(1),
            Self::Enable(e) => {
                w.write_u8(2)?;
                w.write_u8(e.maxburst)?;
                write_count(w, e.formats.len())?;
                e.formats.iter().try_for_each(|f| write_format_spec(w, f))
            }
            Self::Disable => w.write_u8(3),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    /// Whether our function is on the gadget.
    pub enabled: bool,
    pub stream: StreamStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    Error(String),
    Status(Status),
    Ok,
}

impl Response {
    pub fn read_from(r: &mut impl Read) -> io::Result<Self> {
        match r.read_u8()? {
            0 => Ok(Self::Error(read_string(r)?)),
            1 => {
                let enabled = read_bool(r)?;
                let host_speed = match r.read_u8()? {
                    0 => None,
                    s => Some(Speed::from(u32::from(s))),
                };
                let streaming = if read_bool(r)? {
                    Some(StreamParams {
                        kind: read_kind(r)?,
                        width: r.read_u16::<LittleEndian>()?,
                        height: r.read_u16::<LittleEndian>()?,
                        interval: r.read_u32::<LittleEndian>()?,
                        max_frame_size: r.read_u32::<LittleEndian>()?,
                    })
                } else {
                    None
                };
                let fps = f64::from(r.read_u32::<LittleEndian>()?) / 1000.0;
                let failed = u64::from(r.read_u32::<LittleEndian>()?);
                Ok(Self::Status(Status {
                    enabled,
                    stream: StreamStatus {
                        host_speed,
                        streaming,
                        fps,
                        failed,
                    },
                }))
            }
            2 => Ok(Self::Ok),
            id => Err(invalid(format!("Unknown response: {id}"))),
        }
    }

    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        match self {
            Self::Error(message) => {
                w.write_u8(0)?;
                // Error chains can be long; keep the most important start.
                let mut end = message.len().min(u16::MAX.into());
                while !message.is_char_boundary(end) {
                    end -= 1;
                }
                write_string(w, &message[..end])
            }
            Self::Status(s) => {
                w.write_u8(1)?;
                w.write_u8(s.enabled.into())?;
                w.write_u8(speed_to_u8(s.stream.host_speed))?;
                match &s.stream.streaming {
                    Some(p) => {
                        w.write_u8(1)?;
                        write_kind(w, p.kind)?;
                        w.write_u16::<LittleEndian>(p.width)?;
                        w.write_u16::<LittleEndian>(p.height)?;
                        w.write_u32::<LittleEndian>(p.interval)?;
                        w.write_u32::<LittleEndian>(p.max_frame_size)?;
                    }
                    None => w.write_u8(0)?,
                }
                w.write_u32::<LittleEndian>((s.stream.fps * 1000.0).round() as u32)?;
                w.write_u32::<LittleEndian>(s.stream.failed.min(u32::MAX.into()) as u32)
            }
            Self::Ok => w.write_u8(2),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_request(r: &Request) -> Request {
        let mut buf = vec![];
        r.write_to(&mut buf).unwrap();
        let mut cursor = buf.as_slice();
        let parsed = Request::read_from(&mut cursor).unwrap();
        assert!(cursor.is_empty(), "trailing bytes");
        parsed
    }

    fn roundtrip_response(r: &Response) -> Response {
        let mut buf = vec![];
        r.write_to(&mut buf).unwrap();
        let mut cursor = buf.as_slice();
        let parsed = Response::read_from(&mut cursor).unwrap();
        assert!(cursor.is_empty(), "trailing bytes");
        parsed
    }

    #[test]
    fn requests_roundtrip() {
        let enable = Request::Enable(EnableRequest {
            maxburst: 2,
            formats: vec![
                FormatSpec::default_for(FormatKind::Mjpeg).with_fps(&[60, 30]),
                FormatSpec::default_for(FormatKind::Yuyv),
            ],
        });
        for r in [Request::GetStatus, enable, Request::Disable] {
            assert_eq!(roundtrip_request(&r), r);
        }
    }

    #[test]
    fn responses_roundtrip() {
        let status = Response::Status(Status {
            enabled: true,
            stream: StreamStatus {
                host_speed: Some(Speed::Super),
                streaming: Some(StreamParams {
                    kind: FormatKind::Yuyv,
                    width: 640,
                    height: 480,
                    interval: 333_333,
                    max_frame_size: 614_400,
                }),
                fps: 29.958,
                failed: 3,
            },
        });
        for r in [Response::Error("nope".into()), status, Response::Ok] {
            assert_eq!(roundtrip_response(&r), r);
        }

        let idle = Response::Status(Status {
            enabled: false,
            stream: StreamStatus::default(),
        });
        assert_eq!(roundtrip_response(&idle), idle);
    }

    /// The same golden bytes are asserted in the app's `MessageTest.kt`.
    #[test]
    fn golden_encodings_shared_with_app() {
        let enable = Request::Enable(EnableRequest {
            maxburst: 2,
            formats: vec![FormatSpec {
                kind: FormatKind::Mjpeg,
                frames: vec![FrameSpec::new(640, 480, &[30])],
            }],
        });
        let mut buf = vec![];
        enable.write_to(&mut buf).unwrap();
        assert_eq!(
            buf,
            [
                2, 2, 1, 0, 1, 0x80, 0x02, 0xe0, 0x01, 1, 0x15, 0x16, 0x05, 0x00
            ]
        );

        let status = Response::Status(Status {
            enabled: true,
            stream: StreamStatus {
                host_speed: Some(Speed::Super),
                streaming: Some(StreamParams {
                    kind: FormatKind::Yuyv,
                    width: 640,
                    height: 480,
                    interval: 333_333,
                    max_frame_size: 614_400,
                }),
                fps: 29.958,
                failed: 3,
            },
        });
        let mut buf = vec![];
        status.write_to(&mut buf).unwrap();
        #[rustfmt::skip]
        assert_eq!(buf, [
            1, 1, 5, 1, 1, 0x80, 0x02, 0xe0, 0x01, 0x15, 0x16, 0x05, 0x00,
            0x00, 0x60, 0x09, 0x00, 0x06, 0x75, 0x00, 0x00, 0x03, 0, 0, 0,
        ]);
    }

    #[test]
    fn rejects_invalid_requests() {
        let parse = |bytes: &[u8]| Request::read_from(&mut &bytes[..]);

        assert!(parse(&[9]).is_err(), "unknown id");
        assert!(parse(&[2, 16, 1]).is_err(), "maxburst > 15");
        assert!(parse(&[2, 0, 0]).is_err(), "no formats");
        assert!(parse(&[2, 0, 1, 7]).is_err(), "unknown kind");
        // 641x480: odd width.
        assert!(parse(&[2, 0, 1, 0, 1, 0x81, 2, 0xe0, 1, 1, 1, 0, 0, 0]).is_err());
        // Zero interval.
        assert!(parse(&[2, 0, 1, 0, 1, 0x80, 2, 0xe0, 1, 1, 0, 0, 0, 0]).is_err());
        // Two MJPEG formats.
        let spec = [0, 1, 0x80, 2, 0xe0, 1, 1, 1, 0, 0, 0];
        let mut dup = vec![2, 0, 2];
        dup.extend_from_slice(&spec);
        dup.extend_from_slice(&spec);
        assert!(parse(&dup).is_err());
        // Truncated.
        assert!(parse(&[2, 0, 1, 0, 1, 0x80]).is_err());
    }
}

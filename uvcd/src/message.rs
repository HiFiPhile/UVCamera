// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! Daemon protocol, version 4. Mirrored by the app's `daemon/Message.kt`.
//!
//! After connecting, the client sends the protocol version (u8) and the
//! daemon answers 1 (accepted) or 0. Then the client sends requests and
//! reads one response each. All integers are little endian; strings are a
//! u16 length followed by UTF-8. Invalid input closes the connection.
//!
//! ```text
//! Request  := 1 GetStatus
//!           | 2 Enable  maxburst:u8 count:u8 FormatSpec* ControlCaps
//!           | 3 Disable
//!           | 4 AttachSource
//! FormatSpec := kind:u8 count:u8 (width:u16 height:u16 count:u8 interval:u32*)*
//! ControlCaps := flags:u8 [zoom_min:u16 zoom_max:u16]
//!   flags: 1 zoom (followed by its range in hundredths), 2 focus,
//!          4 white balance, 8 power line frequency
//! Response := 0 Error   message:string
//!           | 1 Status  enabled:u8 speed:u8 streaming:u8 [StreamParams]
//!                       fps_milli:u32 source_fps_milli:u32 failed:u32
//!           | 2 Ok
//! StreamParams := kind:u8 width:u16 height:u16 interval:u32 max_frame_size:u32
//! ```
//!
//! `kind` is 0 for MJPEG and 1 for YUYV. `speed` is the kernel's
//! `enum usb_device_speed`, or 0 when no host has configured the function.
//!
//! After `AttachSource` is answered with `Ok`, the connection becomes the
//! frame source channel and carries no more requests:
//!
//! ```text
//! daemon -> app := 1 StreamStart stream_id:u32 StreamParams
//!                | 2 StreamStop  stream_id:u32
//!                | 3 Control     control:u8 value:i32
//! app -> daemon := 1 Frame stream_id:u32 pts_us:u64 len:u32 data[len]
//! ```
//!
//! The app produces frames in the format of the latest `StreamStart` and
//! tags them with its `stream_id`; frames for any other id are dropped.
//! `Control` carries a camera control the host changed (and the current
//! values when the source attaches). Control ids are those of
//! [`ControlId::wire`]: 1 brightness (0..=255, 128 = no change), 2 zoom
//! (hundredths), 3 focus auto, 4 focus (0 nearest .. 1000 infinity), 5 white
//! balance auto, 6 white balance temperature (K), 7 power line frequency
//! (0 off, 1 50 Hz, 2 60 Hz, 3 auto).

use std::io::{self, Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use crate::{
    stream::StreamStatus,
    uvc::{
        Control, StreamParams,
        controls::{ControlCaps, ControlId},
        format::{FormatKind, FormatSpec, FrameSpec},
    },
    v4l2::Speed,
};

pub const PROTOCOL_VERSION: u8 = 4;
/// Abstract Unix socket name.
pub const SOCKET_NAME: &str = "uvcamerad";

const MAX_FORMATS: usize = 2;
const MAX_FRAMES: usize = 32;
const MAX_INTERVALS: usize = 16;
/// Upper bound on one frame from the app, well above 1080p YUYV (4 MB).
pub const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;

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

fn read_stream_params(r: &mut impl Read) -> io::Result<StreamParams> {
    Ok(StreamParams {
        kind: read_kind(r)?,
        width: r.read_u16::<LittleEndian>()?,
        height: r.read_u16::<LittleEndian>()?,
        interval: r.read_u32::<LittleEndian>()?,
        max_frame_size: r.read_u32::<LittleEndian>()?,
    })
}

fn write_stream_params(w: &mut impl Write, p: &StreamParams) -> io::Result<()> {
    write_kind(w, p.kind)?;
    w.write_u16::<LittleEndian>(p.width)?;
    w.write_u16::<LittleEndian>(p.height)?;
    w.write_u32::<LittleEndian>(p.interval)?;
    w.write_u32::<LittleEndian>(p.max_frame_size)
}

fn read_fps(r: &mut impl Read) -> io::Result<f64> {
    Ok(f64::from(r.read_u32::<LittleEndian>()?) / 1000.0)
}

fn write_fps(w: &mut impl Write, fps: f64) -> io::Result<()> {
    w.write_u32::<LittleEndian>((fps * 1000.0).round() as u32)
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
    pub controls: ControlCaps,
}

const CAP_ZOOM: u8 = 1;
const CAP_FOCUS: u8 = 2;
const CAP_WHITE_BALANCE: u8 = 4;
const CAP_POWER_LINE: u8 = 8;

fn read_control_caps(r: &mut impl Read) -> io::Result<ControlCaps> {
    let flags = r.read_u8()?;
    if flags & !(CAP_ZOOM | CAP_FOCUS | CAP_WHITE_BALANCE | CAP_POWER_LINE) != 0 {
        return Err(invalid(format!("Unknown control flags: {flags:#x}")));
    }
    let zoom = if flags & CAP_ZOOM != 0 {
        let min = r.read_u16::<LittleEndian>()?;
        let max = r.read_u16::<LittleEndian>()?;
        if min == 0 || min > max {
            return Err(invalid(format!("Invalid zoom range: {min}..{max}")));
        }
        Some((min, max))
    } else {
        None
    };
    Ok(ControlCaps {
        zoom,
        focus: flags & CAP_FOCUS != 0,
        white_balance: flags & CAP_WHITE_BALANCE != 0,
        power_line: flags & CAP_POWER_LINE != 0,
    })
}

fn write_control_caps(w: &mut impl Write, c: &ControlCaps) -> io::Result<()> {
    let mut flags = 0;
    if c.zoom.is_some() {
        flags |= CAP_ZOOM;
    }
    if c.focus {
        flags |= CAP_FOCUS;
    }
    if c.white_balance {
        flags |= CAP_WHITE_BALANCE;
    }
    if c.power_line {
        flags |= CAP_POWER_LINE;
    }
    w.write_u8(flags)?;
    if let Some((min, max)) = c.zoom {
        w.write_u16::<LittleEndian>(min)?;
        w.write_u16::<LittleEndian>(max)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    GetStatus,
    Enable(EnableRequest),
    Disable,
    /// Turn this connection into the frame source channel.
    AttachSource,
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
                let controls = read_control_caps(r)?;
                Ok(Self::Enable(EnableRequest {
                    maxburst,
                    formats,
                    controls,
                }))
            }
            3 => Ok(Self::Disable),
            4 => Ok(Self::AttachSource),
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
                e.formats.iter().try_for_each(|f| write_format_spec(w, f))?;
                write_control_caps(w, &e.controls)
            }
            Self::Disable => w.write_u8(3),
            Self::AttachSource => w.write_u8(4),
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
                    Some(read_stream_params(r)?)
                } else {
                    None
                };
                let fps = read_fps(r)?;
                let source_fps = read_fps(r)?;
                let failed = u64::from(r.read_u32::<LittleEndian>()?);
                Ok(Self::Status(Status {
                    enabled,
                    stream: StreamStatus {
                        host_speed,
                        streaming,
                        fps,
                        source_fps,
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
                        write_stream_params(w, p)?;
                    }
                    None => w.write_u8(0)?,
                }
                write_fps(w, s.stream.fps)?;
                write_fps(w, s.stream.source_fps)?;
                w.write_u32::<LittleEndian>(s.stream.failed.min(u32::MAX.into()) as u32)
            }
            Self::Ok => w.write_u8(2),
        }
    }
}

/// Daemon-to-app message on the source channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceEvent {
    /// Start producing frames in this format.
    StreamStart {
        stream_id: u32,
        params: StreamParams,
    },
    /// The host stopped streaming; release the camera.
    StreamStop { stream_id: u32 },
    /// The host changed a camera control.
    Control(Control),
}

impl SourceEvent {
    pub fn read_from(r: &mut impl Read) -> io::Result<Self> {
        match r.read_u8()? {
            1 => Ok(Self::StreamStart {
                stream_id: r.read_u32::<LittleEndian>()?,
                params: read_stream_params(r)?,
            }),
            2 => Ok(Self::StreamStop {
                stream_id: r.read_u32::<LittleEndian>()?,
            }),
            3 => {
                let id = r.read_u8()?;
                let value = r.read_i32::<LittleEndian>()?;
                let id = ControlId::from_wire(id)
                    .ok_or_else(|| invalid(format!("Unknown control: {id}")))?;
                Ok(Self::Control(Control { id, value }))
            }
            id => Err(invalid(format!("Unknown source event: {id}"))),
        }
    }

    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        match self {
            Self::StreamStart { stream_id, params } => {
                w.write_u8(1)?;
                w.write_u32::<LittleEndian>(*stream_id)?;
                write_stream_params(w, params)
            }
            Self::StreamStop { stream_id } => {
                w.write_u8(2)?;
                w.write_u32::<LittleEndian>(*stream_id)
            }
            Self::Control(Control { id, value }) => {
                w.write_u8(3)?;
                w.write_u8(id.wire())?;
                w.write_i32::<LittleEndian>(*value)
            }
        }
    }
}

/// App-to-daemon frame on the source channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub stream_id: u32,
    /// Capture time (CLOCK_MONOTONIC, microseconds) for latency tracking.
    pub pts_us: u64,
    pub data: Vec<u8>,
}

impl Frame {
    pub fn read_from(r: &mut impl Read) -> io::Result<Self> {
        match r.read_u8()? {
            1 => {}
            id => return Err(invalid(format!("Unknown source message: {id}"))),
        }
        let stream_id = r.read_u32::<LittleEndian>()?;
        let pts_us = r.read_u64::<LittleEndian>()?;
        let len = r.read_u32::<LittleEndian>()?;
        if len == 0 || len > MAX_FRAME_LEN {
            return Err(invalid(format!("Invalid frame length: {len}")));
        }
        let mut data = vec![0u8; len as usize];
        r.read_exact(&mut data)?;
        Ok(Self {
            stream_id,
            pts_us,
            data,
        })
    }

    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        let len = u32::try_from(self.data.len())
            .ok()
            .filter(|&l| l > 0 && l <= MAX_FRAME_LEN)
            .ok_or_else(|| invalid("Invalid frame length"))?;
        w.write_u8(1)?;
        w.write_u32::<LittleEndian>(self.stream_id)?;
        w.write_u64::<LittleEndian>(self.pts_us)?;
        w.write_u32::<LittleEndian>(len)?;
        w.write_all(&self.data)
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
            controls: ControlCaps {
                zoom: Some((60, 1000)),
                focus: true,
                white_balance: true,
                power_line: true,
            },
        });
        for r in [
            Request::GetStatus,
            enable,
            Request::Disable,
            Request::AttachSource,
        ] {
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
                source_fps: 29.5,
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
            controls: ControlCaps {
                zoom: Some((60, 1000)),
                focus: true,
                white_balance: false,
                power_line: true,
            },
        });
        let mut buf = vec![];
        enable.write_to(&mut buf).unwrap();
        // Formats, then control flags 1|2|8 and the zoom range 60..1000.
        #[rustfmt::skip]
        assert_eq!(buf, [
            2, 2, 1, 0, 1, 0x80, 0x02, 0xe0, 0x01, 1, 0x15, 0x16, 0x05, 0x00,
            0x0b, 60, 0, 0xe8, 0x03,
        ]);

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
                source_fps: 29.5,
                failed: 3,
            },
        });
        let mut buf = vec![];
        status.write_to(&mut buf).unwrap();
        #[rustfmt::skip]
        assert_eq!(buf, [
            1, 1, 5, 1, 1, 0x80, 0x02, 0xe0, 0x01, 0x15, 0x16, 0x05, 0x00,
            0x00, 0x60, 0x09, 0x00, 0x06, 0x75, 0x00, 0x00, 0x3c, 0x73, 0x00, 0x00,
            0x03, 0, 0, 0,
        ]);
    }

    #[test]
    fn source_channel_roundtrip_and_golden() {
        let params = StreamParams {
            kind: FormatKind::Mjpeg,
            width: 1920,
            height: 1080,
            interval: 333_333,
            max_frame_size: 4_147_200,
        };
        for e in [
            SourceEvent::StreamStart {
                stream_id: 7,
                params,
            },
            SourceEvent::StreamStop { stream_id: 7 },
            SourceEvent::Control(Control {
                id: ControlId::Brightness,
                value: 200,
            }),
        ] {
            let mut buf = vec![];
            e.write_to(&mut buf).unwrap();
            assert_eq!(SourceEvent::read_from(&mut buf.as_slice()).unwrap(), e);
        }

        // Same bytes as the app's MessageTest.kt.
        let mut buf = vec![];
        SourceEvent::Control(Control {
            id: ControlId::Brightness,
            value: 200,
        })
        .write_to(&mut buf)
        .unwrap();
        assert_eq!(buf, [3, 1, 200, 0, 0, 0]);

        let frame = Frame {
            stream_id: 7,
            pts_us: 0x0102_0304_0506_0708,
            data: vec![0xff, 0xd8],
        };
        let mut buf = vec![];
        frame.write_to(&mut buf).unwrap();
        #[rustfmt::skip]
        assert_eq!(buf, [
            1, 7, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2, 1, 2, 0, 0, 0, 0xff, 0xd8,
        ]);
        assert_eq!(Frame::read_from(&mut buf.as_slice()).unwrap(), frame);

        // Zero-length and oversized frames are protocol violations.
        assert!(
            Frame::read_from(&mut &[1u8, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0][..])
                .is_err()
        );
        assert!(
            Frame::read_from(&mut &[1u8, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2][..])
                .is_err()
        );
    }

    #[test]
    fn rejects_invalid_control_caps() {
        let spec = [0u8, 1, 0x80, 2, 0xe0, 1, 1, 0x15, 0x16, 0x05, 0x00];
        let enable = |caps: &[u8]| {
            let mut b = vec![2, 0, 1];
            b.extend_from_slice(&spec);
            b.extend_from_slice(caps);
            Request::read_from(&mut b.as_slice())
        };
        assert!(enable(&[0]).is_ok());
        assert!(enable(&[0x10]).is_err(), "unknown flag");
        assert!(enable(&[1, 0, 0, 0xe8, 3]).is_err(), "zoom min 0");
        assert!(enable(&[1, 0xe8, 3, 60, 0]).is_err(), "zoom min > max");
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

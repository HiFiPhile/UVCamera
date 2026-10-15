// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! UVC class request handling, independent of the kernel interface.
//!
//! The f_uvc gadget function forwards every class-specific control request
//! on EP0 to userspace as a `UVC_EVENT_SETUP`, with `wIndex` rewritten so the
//! low byte is 0 for the VideoControl interface and 1 for VideoStreaming
//! (the high byte keeps the entity ID). For host-to-device requests, the data
//! stage arrives later as `UVC_EVENT_DATA`. [`Protocol`] turns those into
//! replies and tracks the PROBE/COMMIT negotiation.

pub mod controls;
pub mod format;

use std::collections::BTreeMap;

use tracing::debug;

use self::{
    controls::{ControlCaps, ControlId, ControlRange, Unit},
    format::{Format, FormatKind, FormatTable, Frame, Interval},
};

// wIndex low byte for our interfaces in kernels that rewrite it (mainline
// 6.2+, Android 5.15 and 6.1); older ones pass the interface number.
const INTERFACE_CONTROL: u8 = 0;
const INTERFACE_STREAMING: u8 = 1;

/// The function's VideoControl and VideoStreaming interface numbers, from
/// configfs (`bInterfaceNumber`), as hosts address them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterfaceNumbers {
    pub control: u8,
    pub streaming: u8,
}

impl Default for InterfaceNumbers {
    fn default() -> Self {
        Self {
            control: INTERFACE_CONTROL,
            streaming: INTERFACE_STREAMING,
        }
    }
}

/// Which interface a request is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Control,
    Streaming,
}

// Request codes (UVC 1.5 table A-8).
pub const SET_CUR: u8 = 0x01;
pub const GET_CUR: u8 = 0x81;
pub const GET_MIN: u8 = 0x82;
pub const GET_MAX: u8 = 0x83;
pub const GET_RES: u8 = 0x84;
pub const GET_LEN: u8 = 0x85;
pub const GET_INFO: u8 = 0x86;
pub const GET_DEF: u8 = 0x87;

// Control selectors.
const VC_REQUEST_ERROR_CODE_CONTROL: u8 = 0x02;
const CT_AE_MODE_CONTROL: u8 = 0x02;
const VS_PROBE_CONTROL: u8 = 0x01;
const VS_COMMIT_CONTROL: u8 = 0x02;

// GET_INFO bits.
const INFO_GET_SET: u8 = 0x03;
const INFO_GET: u8 = 0x01;
const INFO_DISABLED_BY_AUTO: u8 = 0x04;

// AE mode bitmap: auto mode only.
const AE_MODE_AUTO: u8 = 0x02;

/// dwClockFrequency reported in the UVC 1.1 probe structure (48 MHz, the
/// value f_uvc puts in the control header).
const CLOCK_FREQUENCY: u32 = 48_000_000;

/// Request error codes (UVC 1.5 table 4-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorCode {
    None = 0x00,
    InvalidUnit = 0x05,
    InvalidControl = 0x06,
    InvalidRequest = 0x07,
    OutOfRange = 0x04,
}

/// Standard USB SETUP packet as forwarded by f_uvc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CtrlRequest {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl CtrlRequest {
    pub fn from_bytes(b: &[u8; 8]) -> Self {
        Self {
            request_type: b[0],
            request: b[1],
            value: u16::from_le_bytes([b[2], b[3]]),
            index: u16::from_le_bytes([b[4], b[5]]),
            length: u16::from_le_bytes([b[6], b[7]]),
        }
    }

    fn is_in(&self) -> bool {
        self.request_type & 0x80 != 0
    }

    fn interface(&self) -> u8 {
        (self.index & 0xff) as u8
    }

    fn entity(&self) -> u8 {
        (self.index >> 8) as u8
    }

    fn selector(&self) -> u8 {
        (self.value >> 8) as u8
    }
}

/// What to tell the kernel in response to a SETUP event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// Device-to-host data stage. Truncated to `wLength` by the caller.
    Data(Vec<u8>),
    /// Accept a host-to-device data stage of `wLength` bytes; the payload
    /// follows as a DATA event.
    Receive,
    /// Stall EP0.
    Stall,
}

/// Video Probe and Commit Controls (UVC 1.1, 34 bytes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamingControl {
    pub hint: u16,
    pub format_index: u8,
    pub frame_index: u8,
    pub frame_interval: Interval,
    pub key_frame_rate: u16,
    pub p_frame_rate: u16,
    pub comp_quality: u16,
    pub comp_window_size: u16,
    pub delay: u16,
    pub max_video_frame_size: u32,
    pub max_payload_transfer_size: u32,
    pub clock_frequency: u32,
    pub framing_info: u8,
    pub preferred_version: u8,
    pub min_version: u8,
    pub max_version: u8,
}

impl StreamingControl {
    pub const LEN: usize = 34;

    /// Parse a probe/commit payload. UVC 1.0 hosts send 26 bytes; missing
    /// trailing fields are treated as zero and extra (UVC 1.5) bytes ignored.
    pub fn parse(data: &[u8]) -> Self {
        let mut b = [0u8; Self::LEN];
        let n = data.len().min(Self::LEN);
        b[..n].copy_from_slice(&data[..n]);

        let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);

        Self {
            hint: u16_at(0),
            format_index: b[2],
            frame_index: b[3],
            frame_interval: u32_at(4),
            key_frame_rate: u16_at(8),
            p_frame_rate: u16_at(10),
            comp_quality: u16_at(12),
            comp_window_size: u16_at(14),
            delay: u16_at(16),
            max_video_frame_size: u32_at(18),
            max_payload_transfer_size: u32_at(22),
            clock_frequency: u32_at(26),
            framing_info: b[30],
            preferred_version: b[31],
            min_version: b[32],
            max_version: b[33],
        }
    }

    pub fn to_bytes(self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[0..2].copy_from_slice(&self.hint.to_le_bytes());
        b[2] = self.format_index;
        b[3] = self.frame_index;
        b[4..8].copy_from_slice(&self.frame_interval.to_le_bytes());
        b[8..10].copy_from_slice(&self.key_frame_rate.to_le_bytes());
        b[10..12].copy_from_slice(&self.p_frame_rate.to_le_bytes());
        b[12..14].copy_from_slice(&self.comp_quality.to_le_bytes());
        b[14..16].copy_from_slice(&self.comp_window_size.to_le_bytes());
        b[16..18].copy_from_slice(&self.delay.to_le_bytes());
        b[18..22].copy_from_slice(&self.max_video_frame_size.to_le_bytes());
        b[22..26].copy_from_slice(&self.max_payload_transfer_size.to_le_bytes());
        b[26..30].copy_from_slice(&self.clock_frequency.to_le_bytes());
        b[30] = self.framing_info;
        b[31] = self.preferred_version;
        b[32] = self.min_version;
        b[33] = self.max_version;
        b
    }
}

/// The stream the host committed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamParams {
    pub kind: FormatKind,
    pub width: u16,
    pub height: u16,
    pub interval: Interval,
    pub max_frame_size: u32,
}

/// A camera control value the host set, forwarded to the frame source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Control {
    pub id: ControlId,
    pub value: i32,
}

/// What a host-to-device data stage changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataOutcome {
    None,
    /// The host committed a stream format.
    Committed,
    ControlChanged(Control),
}

/// Which control a pending host-to-device data stage is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Probe,
    Commit,
    Control(ControlId),
    AeMode,
}

/// Unit/terminal IDs of the VideoControl entities, read from configfs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityIds {
    pub camera_terminal: u8,
    pub processing_unit: u8,
}

impl Default for EntityIds {
    /// The kernel's defaults.
    fn default() -> Self {
        Self {
            camera_terminal: 1,
            processing_unit: 2,
        }
    }
}

pub struct Protocol {
    formats: FormatTable,
    entities: EntityIds,
    max_payload: u32,
    probe: StreamingControl,
    commit: StreamingControl,
    pending: Option<Pending>,
    error_code: ErrorCode,
    /// Exposed controls and their current values.
    ranges: BTreeMap<ControlId, ControlRange>,
    values: BTreeMap<ControlId, i32>,
    interfaces: InterfaceNumbers,
    /// Whether the kernel rewrites wIndex (see [`INTERFACE_CONTROL`]):
    /// learned from requests, guessed until then.
    rewrites_index: bool,
}

impl Protocol {
    /// `max_payload` is the isochronous endpoint's bytes per service interval
    /// at the connected speed, reported as dwMaxPayloadTransferSize.
    pub fn new(
        formats: FormatTable,
        entities: EntityIds,
        max_payload: u32,
        controls: ControlCaps,
    ) -> Self {
        let (format, frame) = formats.first();
        let default = Self::fill(format, frame, frame.spec.default_interval(), max_payload);
        let ranges = controls.ranges();
        let values = ranges.iter().map(|(&id, r)| (id, r.def)).collect();

        Self {
            formats,
            entities,
            max_payload,
            probe: default,
            commit: default,
            pending: None,
            error_code: ErrorCode::None,
            ranges,
            values,
            interfaces: InterfaceNumbers::default(),
            rewrites_index: true,
        }
    }

    /// The function's interface numbers, and whether the kernel is expected
    /// to rewrite wIndex to [`INTERFACE_CONTROL`]/[`INTERFACE_STREAMING`].
    pub fn set_interfaces(&mut self, interfaces: InterfaceNumbers, rewrites_index: bool) {
        self.interfaces = interfaces;
        self.rewrites_index = rewrites_index;
    }

    /// The interface a request is for, under either wIndex convention.
    fn target(&mut self, req: &CtrlRequest) -> Option<Target> {
        let i = req.interface();
        let InterfaceNumbers { control, streaming } = self.interfaces;

        // Only VideoControl has entities; both conventions keep them.
        if req.entity() != 0 {
            return (i == control || i == INTERFACE_CONTROL).then_some(Target::Control);
        }
        // Numbers only one convention uses tell which one the kernel has.
        if i == streaming && streaming != INTERFACE_STREAMING {
            self.rewrites_index = false;
            return Some(Target::Streaming);
        }
        if i == INTERFACE_CONTROL && control != INTERFACE_CONTROL {
            self.rewrites_index = true;
            return Some(Target::Control);
        }
        match i {
            INTERFACE_CONTROL => Some(Target::Control),
            // The control interface's own number under the old convention.
            INTERFACE_STREAMING if control == INTERFACE_STREAMING => Some(if self.rewrites_index {
                Target::Streaming
            } else {
                Target::Control
            }),
            INTERFACE_STREAMING => Some(Target::Streaming),
            _ if i == control => Some(Target::Control),
            _ => None,
        }
    }

    /// Update the payload size after the link speed is known.
    pub fn set_max_payload(&mut self, max_payload: u32) {
        self.max_payload = max_payload;
        self.probe.max_payload_transfer_size = max_payload;
        self.commit.max_payload_transfer_size = max_payload;
    }

    /// Parameters for the stream the host last committed.
    pub fn committed(&self) -> StreamParams {
        let format = self
            .formats
            .format(self.commit.format_index)
            .expect("validated");
        let frame = format.frame(self.commit.frame_index).expect("validated");
        StreamParams {
            kind: format.kind,
            width: frame.spec.width,
            height: frame.spec.height,
            interval: self.commit.frame_interval,
            max_frame_size: self.commit.max_video_frame_size,
        }
    }

    fn fill(
        format: &Format,
        frame: &Frame,
        interval: Interval,
        max_payload: u32,
    ) -> StreamingControl {
        StreamingControl {
            // dwFrameInterval is fixed by the device.
            hint: 0x0001,
            format_index: format.index,
            frame_index: frame.index,
            frame_interval: interval,
            max_video_frame_size: format
                .kind
                .max_frame_size(frame.spec.width, frame.spec.height),
            max_payload_transfer_size: max_payload,
            clock_frequency: CLOCK_FREQUENCY,
            // Frame ID and end-of-frame bits are used in payload headers.
            framing_info: 0x03,
            preferred_version: 1,
            min_version: 1,
            max_version: 1,
            ..Default::default()
        }
    }

    /// Clamp a host proposal to something we support.
    fn negotiate(&self, req: &StreamingControl) -> StreamingControl {
        let format = self
            .formats
            .format(req.format_index)
            .unwrap_or_else(|| self.formats.first().0);
        let frame = format.frame(req.frame_index).unwrap_or(&format.frames[0]);
        let interval = frame.spec.closest_interval(req.frame_interval);

        Self::fill(format, frame, interval, self.max_payload)
    }

    fn fail(&mut self, code: ErrorCode) -> Reply {
        self.error_code = code;
        Reply::Stall
    }

    fn ok(&mut self, data: Vec<u8>) -> Reply {
        self.error_code = ErrorCode::None;
        Reply::Data(data)
    }

    fn receive(&mut self, pending: Pending) -> Reply {
        self.error_code = ErrorCode::None;
        self.pending = Some(pending);
        Reply::Receive
    }

    pub fn handle_setup(&mut self, req: &CtrlRequest) -> Reply {
        debug!(?req, "Setup request");

        // A new SETUP aborts any data stage we were waiting for.
        self.pending = None;

        if req.is_in() != (req.request != SET_CUR) {
            return self.fail(ErrorCode::InvalidRequest);
        }

        match self.target(req) {
            Some(Target::Streaming) => self.handle_streaming(req),
            Some(Target::Control) => match req.entity() {
                0 => self.handle_interface_control(req),
                id if id == self.entities.camera_terminal => self.handle_camera_terminal(req),
                id if id == self.entities.processing_unit => self.handle_processing_unit(req),
                _ => self.fail(ErrorCode::InvalidUnit),
            },
            None => self.fail(ErrorCode::InvalidUnit),
        }
    }

    /// Restore a control value, e.g. when the video node is reopened.
    pub fn set_control(&mut self, control: Control) {
        if self
            .ranges
            .get(&control.id)
            .is_some_and(|r| r.contains(control.value))
        {
            self.values.insert(control.id, control.value);
        }
    }

    /// Host-to-device data stage of the last SETUP.
    pub fn handle_data(&mut self, data: &[u8]) -> DataOutcome {
        debug!(?data, "Setup data");

        match self.pending.take() {
            Some(Pending::Probe) => {
                self.probe = self.negotiate(&StreamingControl::parse(data));
                debug!(probe = ?self.probe, "Probe");
                DataOutcome::None
            }
            Some(Pending::Commit) => {
                self.commit = self.negotiate(&StreamingControl::parse(data));
                debug!(commit = ?self.commit, "Commit");
                DataOutcome::Committed
            }
            Some(Pending::Control(id)) => {
                let value = id.decode(data).filter(|&v| self.ranges[&id].contains(v));
                match value {
                    None => {
                        self.error_code = ErrorCode::OutOfRange;
                        DataOutcome::None
                    }
                    Some(v) if self.values.get(&id) == Some(&v) => DataOutcome::None,
                    Some(value) => {
                        self.values.insert(id, value);
                        DataOutcome::ControlChanged(Control { id, value })
                    }
                }
            }
            Some(Pending::AeMode) if !data.is_empty() => {
                if data[0] != AE_MODE_AUTO {
                    self.error_code = ErrorCode::OutOfRange;
                }
                DataOutcome::None
            }
            _ => DataOutcome::None,
        }
    }

    fn handle_streaming(&mut self, req: &CtrlRequest) -> Reply {
        let (pending, cur) = match req.selector() {
            VS_PROBE_CONTROL => (Pending::Probe, self.probe),
            VS_COMMIT_CONTROL => (Pending::Commit, self.commit),
            _ => return self.fail(ErrorCode::InvalidControl),
        };

        let control = match req.request {
            SET_CUR => return self.receive(pending),
            GET_CUR => cur,
            GET_MIN | GET_DEF => {
                let (format, frame) = self.formats.first();
                Self::fill(
                    format,
                    frame,
                    frame.spec.default_interval(),
                    self.max_payload,
                )
            }
            GET_MAX => {
                let (format, frame) = self.formats.last();
                Self::fill(
                    format,
                    frame,
                    *frame.spec.intervals.last().unwrap(),
                    self.max_payload,
                )
            }
            GET_RES => StreamingControl::default(),
            GET_LEN => return self.ok((StreamingControl::LEN as u16).to_le_bytes().to_vec()),
            GET_INFO => return self.ok(vec![INFO_GET_SET]),
            _ => return self.fail(ErrorCode::InvalidRequest),
        };

        self.ok(control.to_bytes().to_vec())
    }

    fn handle_interface_control(&mut self, req: &CtrlRequest) -> Reply {
        if req.selector() != VC_REQUEST_ERROR_CODE_CONTROL {
            return self.fail(ErrorCode::InvalidControl);
        }

        // Reading the error code must not reset it.
        match req.request {
            GET_CUR => Reply::Data(vec![self.error_code as u8]),
            GET_INFO => Reply::Data(vec![INFO_GET]),
            GET_LEN => Reply::Data(1u16.to_le_bytes().to_vec()),
            _ => self.fail(ErrorCode::InvalidRequest),
        }
    }

    fn handle_camera_terminal(&mut self, req: &CtrlRequest) -> Reply {
        if req.selector() != CT_AE_MODE_CONTROL {
            return self.handle_control(Unit::CameraTerminal, req);
        }

        match req.request {
            SET_CUR => self.receive(Pending::AeMode),
            // For AE mode, GET_RES is the bitmap of supported modes.
            GET_CUR | GET_DEF | GET_RES => self.ok(vec![AE_MODE_AUTO]),
            GET_LEN => self.ok(1u16.to_le_bytes().to_vec()),
            GET_INFO => self.ok(vec![INFO_GET_SET]),
            _ => self.fail(ErrorCode::InvalidRequest),
        }
    }

    fn handle_processing_unit(&mut self, req: &CtrlRequest) -> Reply {
        self.handle_control(Unit::ProcessingUnit, req)
    }

    /// GET/SET of a control from the [`ControlCaps`] table.
    fn handle_control(&mut self, unit: Unit, req: &CtrlRequest) -> Reply {
        let Some(id) = ControlId::from_selector(unit, req.selector())
            .filter(|id| self.ranges.contains_key(id))
        else {
            return self.fail(ErrorCode::InvalidControl);
        };
        let range = self.ranges[&id];

        let value = match req.request {
            SET_CUR => return self.receive(Pending::Control(id)),
            GET_CUR => self.values[&id],
            GET_MIN => range.min,
            GET_MAX => range.max,
            GET_RES => range.res,
            GET_DEF => range.def,
            GET_LEN => return self.ok((id.len() as u16).to_le_bytes().to_vec()),
            GET_INFO => {
                // Tell the host a manual control is disabled while its auto
                // control is on (UVC 1.5 table 4-3, D2).
                let auto_on = id
                    .auto_control()
                    .is_some_and(|auto| self.values.get(&auto) == Some(&1));
                return self.ok(vec![
                    INFO_GET_SET | if auto_on { INFO_DISABLED_BY_AUTO } else { 0 },
                ]);
            }
            _ => return self.fail(ErrorCode::InvalidRequest),
        };

        self.ok(id.encode(value))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        format::{FormatSpec, FrameSpec},
        *,
    };

    const PU_BRIGHTNESS_CONTROL: u8 = 0x02;

    const PAYLOAD: u32 = 3072;

    fn protocol() -> Protocol {
        let table = FormatTable::from_specs(&[
            FormatSpec::default_for(FormatKind::Mjpeg),
            FormatSpec {
                kind: FormatKind::Yuyv,
                frames: vec![FrameSpec::new(640, 480, &[30, 15])],
            },
        ])
        .unwrap();
        Protocol::new(table, EntityIds::default(), PAYLOAD, ControlCaps::default())
    }

    fn req(request_type: u8, request: u8, selector: u8, index: u16, length: u16) -> CtrlRequest {
        CtrlRequest {
            request_type,
            request,
            value: u16::from(selector) << 8,
            index,
            length,
        }
    }

    fn get(request: u8, selector: u8, index: u16) -> CtrlRequest {
        req(0xa1, request, selector, index, 34)
    }

    fn set(selector: u8, index: u16) -> CtrlRequest {
        req(0x21, SET_CUR, selector, index, 34)
    }

    fn data(reply: Reply) -> Vec<u8> {
        match reply {
            Reply::Data(d) => d,
            r => panic!("Expected data, got {r:?}"),
        }
    }

    /// GKI 5.10 passes interface numbers through: with MTP as interface 0,
    /// control is 1 and streaming 2.
    #[test]
    fn raw_interface_numbers() {
        let mut p = protocol();
        p.set_interfaces(
            InterfaceNumbers {
                control: 1,
                streaming: 2,
            },
            false,
        );
        let probe =
            StreamingControl::parse(&data(p.handle_setup(&get(GET_CUR, VS_PROBE_CONTROL, 2))));
        assert_eq!(probe.format_index, 1);
        // Processing unit 2 on interface 1.
        assert_eq!(
            data(p.handle_setup(&req(0xa1, GET_CUR, PU_BRIGHTNESS_CONTROL, 0x0201, 2))),
            vec![128, 0]
        );
        // The control interface's error code, at interface 1.
        assert_eq!(
            data(p.handle_setup(&req(0xa1, GET_CUR, VC_REQUEST_ERROR_CODE_CONTROL, 1, 1))),
            vec![0]
        );
        assert_eq!(
            p.handle_setup(&get(GET_CUR, VS_PROBE_CONTROL, 3)),
            Reply::Stall
        );
    }

    /// Kernels that rewrite wIndex send 0 and 1 whatever the numbers are.
    #[test]
    fn rewritten_interface_numbers() {
        let mut p = protocol();
        // Guessed wrong at first; the first request at 0 corrects it.
        p.set_interfaces(
            InterfaceNumbers {
                control: 1,
                streaming: 2,
            },
            false,
        );
        assert_eq!(
            data(p.handle_setup(&req(0xa1, GET_CUR, VC_REQUEST_ERROR_CODE_CONTROL, 0, 1))),
            vec![0]
        );
        let probe =
            StreamingControl::parse(&data(p.handle_setup(&get(GET_CUR, VS_PROBE_CONTROL, 1))));
        assert_eq!(probe.format_index, 1);
        assert_eq!(
            data(p.handle_setup(&req(0xa1, GET_CUR, PU_BRIGHTNESS_CONTROL, 0x0200, 2))),
            vec![128, 0]
        );
    }

    #[test]
    fn streaming_control_roundtrip() {
        let control = StreamingControl {
            hint: 1,
            format_index: 2,
            frame_index: 3,
            frame_interval: 333_333,
            max_video_frame_size: 614_400,
            max_payload_transfer_size: 3072,
            clock_frequency: 48_000_000,
            framing_info: 3,
            preferred_version: 1,
            min_version: 1,
            max_version: 1,
            ..Default::default()
        };
        assert_eq!(StreamingControl::parse(&control.to_bytes()), control);

        // UVC 1.0 hosts send the 26 byte variant.
        let short = StreamingControl::parse(&control.to_bytes()[..26]);
        assert_eq!(short.frame_interval, 333_333);
        assert_eq!(short.clock_frequency, 0);
    }

    /// Typical Windows/Linux sequence: GET_CUR, SET_CUR probe, GET_CUR probe,
    /// SET_CUR commit.
    #[test]
    fn probe_commit_negotiation() {
        let mut p = protocol();

        let initial =
            StreamingControl::parse(&data(p.handle_setup(&get(GET_CUR, VS_PROBE_CONTROL, 1))));
        assert_eq!((initial.format_index, initial.frame_index), (1, 1));
        assert_eq!(initial.max_payload_transfer_size, PAYLOAD);

        // Ask for YUYV 640x480 at an unsupported 20 fps: clamps to 15 fps.
        assert_eq!(p.handle_setup(&set(VS_PROBE_CONTROL, 1)), Reply::Receive);
        let proposal = StreamingControl {
            format_index: 2,
            frame_index: 1,
            frame_interval: 500_000,
            ..Default::default()
        };
        assert_eq!(p.handle_data(&proposal.to_bytes()), DataOutcome::None);

        let probe =
            StreamingControl::parse(&data(p.handle_setup(&get(GET_CUR, VS_PROBE_CONTROL, 1))));
        assert_eq!((probe.format_index, probe.frame_index), (2, 1));
        assert_eq!(probe.frame_interval, 666_666);
        assert_eq!(probe.max_video_frame_size, 640 * 480 * 2);

        assert_eq!(p.handle_setup(&set(VS_COMMIT_CONTROL, 1)), Reply::Receive);
        assert_eq!(p.handle_data(&probe.to_bytes()), DataOutcome::Committed);
        assert_eq!(
            p.committed(),
            StreamParams {
                kind: FormatKind::Yuyv,
                width: 640,
                height: 480,
                interval: 666_666,
                max_frame_size: 640 * 480 * 2,
            }
        );
    }

    #[test]
    fn invalid_indexes_fall_back_to_first() {
        let mut p = protocol();
        p.handle_setup(&set(VS_PROBE_CONTROL, 1));
        p.handle_data(
            &StreamingControl {
                format_index: 9,
                frame_index: 9,
                ..Default::default()
            }
            .to_bytes(),
        );
        let probe =
            StreamingControl::parse(&data(p.handle_setup(&get(GET_CUR, VS_PROBE_CONTROL, 1))));
        assert_eq!(
            (probe.format_index, probe.frame_index, probe.frame_interval),
            (1, 1, 333_333)
        );
    }

    #[test]
    fn min_max_def() {
        let mut p = protocol();
        let max =
            StreamingControl::parse(&data(p.handle_setup(&get(GET_MAX, VS_PROBE_CONTROL, 1))));
        assert_eq!(
            (max.format_index, max.frame_index, max.frame_interval),
            (2, 1, 666_666)
        );
        let def =
            StreamingControl::parse(&data(p.handle_setup(&get(GET_DEF, VS_PROBE_CONTROL, 1))));
        assert_eq!((def.format_index, def.frame_index), (1, 1));
        assert_eq!(
            data(p.handle_setup(&get(GET_INFO, VS_PROBE_CONTROL, 1))),
            vec![INFO_GET_SET]
        );
        assert_eq!(
            data(p.handle_setup(&get(GET_LEN, VS_PROBE_CONTROL, 1))),
            vec![34, 0]
        );
    }

    #[test]
    fn brightness_and_error_code() {
        let mut p = protocol();
        let pu = 2 << 8;
        assert_eq!(
            data(p.handle_setup(&get(GET_DEF, PU_BRIGHTNESS_CONTROL, pu))),
            128i16.to_le_bytes()
        );

        assert_eq!(
            p.handle_setup(&set(PU_BRIGHTNESS_CONTROL, pu)),
            Reply::Receive
        );
        assert_eq!(
            p.handle_data(&200i16.to_le_bytes()),
            DataOutcome::ControlChanged(Control {
                id: ControlId::Brightness,
                value: 200
            })
        );
        // Setting the same value again is not a change.
        p.handle_setup(&set(PU_BRIGHTNESS_CONTROL, pu));
        assert_eq!(p.handle_data(&200i16.to_le_bytes()), DataOutcome::None);
        // Out of range is rejected with an error code.
        p.handle_setup(&set(PU_BRIGHTNESS_CONTROL, pu));
        assert_eq!(p.handle_data(&300i16.to_le_bytes()), DataOutcome::None);
        assert_eq!(
            data(p.handle_setup(&get(GET_CUR, PU_BRIGHTNESS_CONTROL, pu))),
            200i16.to_le_bytes()
        );

        // Unknown control stalls and records the reason.
        assert_eq!(p.handle_setup(&get(GET_CUR, 0x03, pu)), Reply::Stall);
        let error = get(GET_CUR, VC_REQUEST_ERROR_CODE_CONTROL, 0);
        assert_eq!(
            data(p.handle_setup(&error)),
            vec![ErrorCode::InvalidControl as u8]
        );
        // Reading it does not clear it.
        assert_eq!(
            data(p.handle_setup(&error)),
            vec![ErrorCode::InvalidControl as u8]
        );

        // Unknown entity.
        assert_eq!(p.handle_setup(&get(GET_CUR, 0x02, 7 << 8)), Reply::Stall);
        assert_eq!(
            data(p.handle_setup(&error)),
            vec![ErrorCode::InvalidUnit as u8]
        );
    }

    #[test]
    fn advertised_controls_get_set_and_auto_info() {
        let table = FormatTable::from_specs(&[FormatSpec::default_for(FormatKind::Mjpeg)]).unwrap();
        let caps = ControlCaps {
            zoom: Some((60, 1000)),
            focus: true,
            white_balance: true,
            power_line: true,
        };
        let mut p = Protocol::new(table, EntityIds::default(), PAYLOAD, caps);
        let ct = 1 << 8;
        let pu = 2 << 8;
        let get2 = |r, sel, idx| req(0xa1, r, sel, idx, 2);

        // Zoom: range from the camera, default 1x.
        assert_eq!(
            data(p.handle_setup(&get2(GET_MIN, 0x0b, ct))),
            60u16.to_le_bytes()
        );
        assert_eq!(
            data(p.handle_setup(&get2(GET_MAX, 0x0b, ct))),
            1000u16.to_le_bytes()
        );
        assert_eq!(
            data(p.handle_setup(&get2(GET_CUR, 0x0b, ct))),
            100u16.to_le_bytes()
        );
        assert_eq!(
            p.handle_setup(&req(0x21, SET_CUR, 0x0b, ct, 2)),
            Reply::Receive
        );
        assert_eq!(
            p.handle_data(&300u16.to_le_bytes()),
            DataOutcome::ControlChanged(Control {
                id: ControlId::Zoom,
                value: 300
            })
        );
        p.handle_setup(&req(0x21, SET_CUR, 0x0b, ct, 2));
        assert_eq!(p.handle_data(&2000u16.to_le_bytes()), DataOutcome::None);
        assert_eq!(
            data(p.handle_setup(&get2(GET_CUR, 0x0b, ct))),
            300u16.to_le_bytes()
        );

        // Focus is disabled while autofocus is on (default), enabled after.
        assert_eq!(
            data(p.handle_setup(&req(0xa1, GET_INFO, 0x06, ct, 1))),
            vec![0x07]
        );
        p.handle_setup(&req(0x21, SET_CUR, 0x08, ct, 1));
        p.handle_data(&[0]);
        assert_eq!(
            data(p.handle_setup(&req(0xa1, GET_INFO, 0x06, ct, 1))),
            vec![0x03]
        );

        // Processing unit: 1-byte power line, 2-byte WB temperature.
        assert_eq!(
            data(p.handle_setup(&req(0xa1, GET_DEF, 0x05, pu, 1))),
            vec![3]
        );
        assert_eq!(
            data(p.handle_setup(&req(0xa1, GET_LEN, 0x05, pu, 2))),
            vec![1, 0]
        );
        assert_eq!(
            data(p.handle_setup(&get2(GET_RES, 0x0a, pu))),
            100u16.to_le_bytes()
        );
    }

    #[test]
    fn unadvertised_controls_stall() {
        let mut p = protocol();
        // Zoom isn't in the default caps.
        assert_eq!(
            p.handle_setup(&req(0xa1, GET_CUR, 0x0b, 1 << 8, 2)),
            Reply::Stall
        );
    }

    #[test]
    fn camera_terminal_ae_mode() {
        let mut p = protocol();
        let ct = 1 << 8;
        assert_eq!(
            data(p.handle_setup(&get(GET_CUR, CT_AE_MODE_CONTROL, ct))),
            vec![AE_MODE_AUTO]
        );
        assert_eq!(
            data(p.handle_setup(&get(GET_RES, CT_AE_MODE_CONTROL, ct))),
            vec![AE_MODE_AUTO]
        );
    }

    #[test]
    fn direction_mismatch_stalls() {
        let mut p = protocol();
        // GET_CUR with an OUT request type.
        assert_eq!(
            p.handle_setup(&req(0x21, GET_CUR, VS_PROBE_CONTROL, 1, 34)),
            Reply::Stall
        );
    }

    /// Requests captured from the Windows 11 Camera app on first open, in
    /// order, as (bmRequestType, bRequest, wValue, wIndex, wLength). Windows
    /// probes controls we don't advertise and checks the error code after
    /// each stall, then negotiates MJPEG 1920x1080@30.
    #[test]
    fn windows_11_camera_app_sequence() {
        #[rustfmt::skip]
        const PROBE: [u8; 34] = [
            1, 0, 1, 1, 21, 22, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 72, 63, 0, 0, 0, 0, 0, 0, 108, 220, 2, 3, 1, 1, 1,
        ];
        #[rustfmt::skip]
        const COMMIT: [u8; 34] = [
            1, 0, 1, 1, 21, 22, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 72, 63, 0, 0, 12, 0, 0, 0, 108, 220, 2, 3, 1, 1, 1,
        ];

        let table = FormatTable::from_specs(&[
            FormatSpec::default_for(FormatKind::Mjpeg),
            FormatSpec::default_for(FormatKind::Yuyv),
        ])
        .unwrap();
        let mut p = Protocol::new(table, EntityIds::default(), PAYLOAD, ControlCaps::default());

        let r = |t, r, v, i, l| CtrlRequest {
            request_type: t,
            request: r,
            value: v,
            index: i,
            length: l,
        };
        let error_code = r(161, GET_CUR, 512, 0, 1);

        // CT exposure time absolute and iris: not advertised, so stall.
        for selector in [0x04, 0x09] {
            assert_eq!(
                p.handle_setup(&r(161, GET_INFO, selector << 8, 256, 1)),
                Reply::Stall
            );
            assert_eq!(
                data(p.handle_setup(&error_code)),
                vec![ErrorCode::InvalidControl as u8]
            );
        }

        // PU brightness range and current value.
        assert_eq!(
            data(p.handle_setup(&r(161, GET_INFO, 512, 512, 1))),
            vec![INFO_GET_SET]
        );
        for (request, value) in [
            (GET_MIN, 0i16),
            (GET_MAX, 255),
            (GET_RES, 1),
            (GET_DEF, 128),
        ] {
            assert_eq!(
                data(p.handle_setup(&r(161, request, 512, 512, 2))),
                value.to_le_bytes()
            );
        }
        for _ in 0..7 {
            assert_eq!(
                data(p.handle_setup(&r(161, GET_CUR, 512, 512, 2))),
                128i16.to_le_bytes()
            );
        }

        // Probe/commit.
        p.handle_setup(&r(161, GET_CUR, 256, 1, 34));
        assert_eq!(p.handle_setup(&r(33, SET_CUR, 256, 1, 34)), Reply::Receive);
        assert_eq!(p.handle_data(&PROBE), DataOutcome::None);
        let probe = StreamingControl::parse(&data(p.handle_setup(&r(161, GET_CUR, 256, 1, 34))));
        assert_eq!(
            (probe.format_index, probe.frame_index, probe.frame_interval),
            (1, 1, 333_333)
        );
        assert_eq!(probe.max_payload_transfer_size, PAYLOAD);
        p.handle_setup(&r(161, GET_MAX, 256, 1, 34));
        p.handle_setup(&r(161, GET_MIN, 256, 1, 34));
        p.handle_setup(&r(33, SET_CUR, 256, 1, 34));
        p.handle_data(&PROBE);
        p.handle_setup(&r(161, GET_CUR, 256, 1, 34));

        assert_eq!(p.handle_setup(&r(33, SET_CUR, 512, 1, 34)), Reply::Receive);
        assert_eq!(p.handle_data(&COMMIT), DataOutcome::Committed);
        assert_eq!(
            p.committed(),
            StreamParams {
                kind: FormatKind::Mjpeg,
                width: 1920,
                height: 1080,
                interval: 333_333,
                max_frame_size: 1920 * 1080 * 2,
            }
        );
    }

    /// Requests captured from Linux `uvcvideo` (`v4l2-ctl --list-formats-ext`
    /// then `ffplay` at 1080p60), as (bmRequestType, bRequest, wValue,
    /// wIndex, wLength). Linux reads the control ranges at probe time, then
    /// probes every format/frame with SET_CUR, GET_MIN, GET_MAX, SET_CUR,
    /// GET_CUR before committing.
    #[test]
    fn linux_uvcvideo_sequence() {
        let table = FormatTable::from_specs(&[
            FormatSpec::default_for(FormatKind::Mjpeg).with_fps(&[60, 30, 15]),
            FormatSpec::default_for(FormatKind::Yuyv),
        ])
        .unwrap();
        let mut p = Protocol::new(table, EntityIds::default(), PAYLOAD, ControlCaps::default());

        let r = |t, r, v, i, l| CtrlRequest {
            request_type: t,
            request: r,
            value: v,
            index: i,
            length: l,
        };
        let probe = |format: u8, frame: u8, interval: u32| {
            StreamingControl {
                hint: 1,
                format_index: format,
                frame_index: frame,
                frame_interval: interval,
                ..Default::default()
            }
            .to_bytes()
        };

        // Control initialization.
        assert_eq!(
            data(p.handle_setup(&r(161, GET_INFO, 512, 512, 1))),
            vec![INFO_GET_SET]
        );
        assert_eq!(
            data(p.handle_setup(&r(161, GET_INFO, 512, 256, 1))),
            vec![INFO_GET_SET]
        );

        // Initial probe with the default format: 1080p60.
        let def = StreamingControl::parse(&data(p.handle_setup(&r(161, GET_DEF, 256, 1, 34))));
        assert_eq!(
            (def.format_index, def.frame_index, def.frame_interval),
            (1, 1, 166_666)
        );

        for (request, value) in [
            (GET_DEF, 128i16),
            (GET_MIN, 0),
            (GET_MAX, 255),
            (GET_RES, 1),
        ] {
            assert_eq!(
                data(p.handle_setup(&r(161, request, 512, 512, 2))),
                value.to_le_bytes()
            );
        }
        assert_eq!(
            data(p.handle_setup(&r(161, GET_DEF, 512, 256, 1))),
            vec![AE_MODE_AUTO]
        );
        assert_eq!(
            data(p.handle_setup(&r(161, GET_RES, 512, 256, 1))),
            vec![AE_MODE_AUTO]
        );

        // Format enumeration: every frame of both formats, including YUYV
        // 1280x720 at its only rate (10 fps = 1000000).
        for (format, frame, interval) in [
            (1, 1, 166_666),
            (1, 2, 166_666),
            (1, 3, 166_666),
            (2, 1, 333_333),
            (2, 2, 1_000_000),
        ] {
            assert_eq!(p.handle_setup(&r(33, SET_CUR, 256, 1, 34)), Reply::Receive);
            p.handle_data(&probe(format, frame, interval));
            p.handle_setup(&r(161, GET_MIN, 256, 1, 34));
            p.handle_setup(&r(161, GET_MAX, 256, 1, 34));
            p.handle_setup(&r(33, SET_CUR, 256, 1, 34));
            p.handle_data(&probe(format, frame, interval));
            let cur = StreamingControl::parse(&data(p.handle_setup(&r(161, GET_CUR, 256, 1, 34))));
            assert_eq!(
                (cur.format_index, cur.frame_index, cur.frame_interval),
                (format, frame, interval)
            );
        }

        // Commit MJPEG 1080p60.
        assert_eq!(p.handle_setup(&r(33, SET_CUR, 512, 1, 34)), Reply::Receive);
        assert_eq!(p.handle_data(&probe(1, 1, 166_666)), DataOutcome::Committed);
        assert_eq!(
            p.committed(),
            StreamParams {
                kind: FormatKind::Mjpeg,
                width: 1920,
                height: 1080,
                interval: 166_666,
                max_frame_size: 1920 * 1080 * 2,
            }
        );
    }
}

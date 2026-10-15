// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! Hand-off between the frame source (the app's attached connection) and
//! the stream thread.
//!
//! At most one source is attached. The stream thread announces each host
//! stream with a fresh id; the source thread stores frames tagged with the
//! current id. Formats whose frames stand alone keep only the newest one.
//! H.264 frames depend on each other, so they queue in order; if the queue
//! overflows, frames are dropped up to the next IDR frame, which the source
//! is asked for. Writes to the app use a short timeout so a stuck app can
//! never stall the USB stream.

use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, Write},
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::net::UnixStream,
    },
    sync::Mutex,
    time::{Duration, Instant},
};

use rustix::event::{EventfdFlags, eventfd};
use tracing::{debug, info, warn};

use crate::{
    message::{Frame, SourceEvent},
    uvc::{Control, StreamParams, controls::ControlId},
};

const WRITE_TIMEOUT: Duration = Duration::from_millis(200);
/// H.264 frames waiting for a free buffer before the queue counts as
/// overflowing: a few frame intervals.
const MAX_QUEUED: usize = 8;

/// Whether an H.264 access unit (Annex B) starts a new coded video sequence:
/// it contains an SPS or an IDR slice.
pub fn is_h264_key_frame(data: &[u8]) -> bool {
    let mut zeros = 0;
    for (i, &b) in data.iter().enumerate() {
        // nal_unit_type: 5 IDR slice, 7 SPS.
        if b == 1
            && zeros >= 2
            && data
                .get(i + 1)
                .is_some_and(|&header| matches!(header & 0x1f, 5 | 7))
        {
            return true;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    false
}

struct Attached {
    id: u64,
    writer: UnixStream,
}

#[derive(Default)]
struct Inner {
    source: Option<Attached>,
    next_source_id: u64,
    /// The host stream currently being served.
    stream: Option<(u32, StreamParams)>,
    next_stream_id: u32,
    /// Frames for the current stream that the stream thread hasn't taken
    /// yet: at most one unless the stream is H.264.
    pending: VecDeque<(Frame, Instant)>,
    /// H.264: frames are dropped until the next key frame.
    need_key_frame: bool,
    /// Control values the host set in the current session.
    controls: BTreeMap<ControlId, i32>,
}

impl Inner {
    /// Send an event to the source, detaching it if it can't keep up.
    fn send(&mut self, event: &SourceEvent) {
        let Some(source) = &mut self.source else {
            return;
        };

        let mut buf = vec![];
        event
            .write_to(&mut buf)
            .expect("Writing to a Vec cannot fail");
        if let Err(e) = source.writer.write_all(&buf) {
            warn!("Detaching frame source after failed write: {e}");
            let _ = source.writer.shutdown(std::net::Shutdown::Both);
            self.source = None;
        }
    }
}

pub struct SourceHub {
    inner: Mutex<Inner>,
    /// Signaled whenever a frame for the current stream arrives, so the
    /// stream thread can queue it right away.
    notify: OwnedFd,
}

impl Default for SourceHub {
    fn default() -> Self {
        Self {
            inner: Mutex::default(),
            notify: eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
                .expect("Failed to create eventfd"),
        }
    }
}

impl SourceHub {
    /// Readable when a new frame may be available; see [`Self::clear_notify`].
    pub fn notify_fd(&self) -> BorrowedFd<'_> {
        self.notify.as_fd()
    }

    /// Reset the notification before taking frames.
    pub fn clear_notify(&self) {
        let mut buf = [0u8; 8];
        let _ = rustix::io::read(&self.notify, &mut buf);
    }
    /// Make `writer`'s connection the frame source, replacing any previous
    /// one. If the host is already streaming, the new source is told
    /// immediately. Returns an id for [`Self::detach`].
    pub fn attach(&self, writer: UnixStream) -> io::Result<u64> {
        writer.set_write_timeout(Some(WRITE_TIMEOUT))?;

        let mut inner = self.inner.lock().unwrap();
        if let Some(old) = inner.source.take() {
            info!("Replacing frame source {}", old.id);
            let _ = old.writer.shutdown(std::net::Shutdown::Both);
        }

        let id = inner.next_source_id;
        inner.next_source_id += 1;
        inner.source = Some(Attached { id, writer });
        info!("Frame source {id} attached");

        // Bring the new source up to date.
        let controls: Vec<_> = inner
            .controls
            .iter()
            .map(|(&id, &value)| Control { id, value })
            .collect();
        for control in controls {
            inner.send(&SourceEvent::Control(control));
        }
        if let Some((stream_id, params)) = inner.stream {
            inner.send(&SourceEvent::StreamStart { stream_id, params });
        }
        Ok(id)
    }

    /// Detach the source with this id, if it is still the current one.
    pub fn detach(&self, id: u64) {
        let mut inner = self.inner.lock().unwrap();
        if inner.source.as_ref().is_some_and(|s| s.id == id) {
            inner.source = None;
            inner.pending.clear();
            info!("Frame source {id} detached");
        }
    }

    #[cfg(test)]
    pub fn is_attached(&self) -> bool {
        self.inner.lock().unwrap().source.is_some()
    }

    /// The host started a stream. Returns its id.
    pub fn stream_started(&self, params: StreamParams) -> u32 {
        let mut inner = self.inner.lock().unwrap();
        let stream_id = inner.next_stream_id;
        inner.next_stream_id = inner.next_stream_id.wrapping_add(1);
        inner.stream = Some((stream_id, params));
        inner.pending.clear();
        inner.need_key_frame = !params.kind.is_intra_only();
        inner.send(&SourceEvent::StreamStart { stream_id, params });
        stream_id
    }

    /// The host changed a control: remember it and tell the source.
    pub fn control_changed(&self, control: Control) {
        let mut inner = self.inner.lock().unwrap();
        inner.controls.insert(control.id, control.value);
        inner.send(&SourceEvent::Control(control));
    }

    /// Forget control values, e.g. when the webcam is enabled again with
    /// possibly another camera.
    pub fn reset_controls(&self) {
        self.inner.lock().unwrap().controls.clear();
    }

    /// Current control values, for restoring them when the video node is
    /// reopened.
    pub fn controls(&self) -> Vec<Control> {
        let inner = self.inner.lock().unwrap();
        inner
            .controls
            .iter()
            .map(|(&id, &value)| Control { id, value })
            .collect()
    }

    /// The host stopped streaming.
    pub fn stream_stopped(&self) {
        let mut inner = self.inner.lock().unwrap();
        if let Some((stream_id, _)) = inner.stream.take() {
            inner.pending.clear();
            inner.send(&SourceEvent::StreamStop { stream_id });
        }
    }

    /// Store a frame from the source with this id. Frames for other
    /// streams, or from a replaced source, are dropped.
    pub fn submit(&self, source_id: u64, frame: Frame) {
        let mut inner = self.inner.lock().unwrap();
        if inner.source.as_ref().is_none_or(|s| s.id != source_id) {
            return;
        }
        let Some((stream_id, params)) = inner.stream else {
            debug!("Dropping frame for stream {}", frame.stream_id);
            return;
        };
        if stream_id != frame.stream_id {
            debug!("Dropping frame for stream {}", frame.stream_id);
            return;
        }

        if params.kind.is_intra_only() {
            inner.pending.clear();
        } else {
            if inner.need_key_frame {
                if !is_h264_key_frame(&frame.data) {
                    return;
                }
                inner.need_key_frame = false;
            }
            if inner.pending.len() >= MAX_QUEUED {
                // The host can't keep up: resynchronize at the next IDR.
                warn!(
                    "Dropping {} queued H.264 frames; requesting a key frame",
                    inner.pending.len()
                );
                inner.pending.clear();
                inner.need_key_frame = true;
                inner.send(&SourceEvent::KeyFrame { stream_id });
                return;
            }
        }
        inner.pending.push_back((frame, Instant::now()));
        let _ = rustix::io::write(&self.notify, &1u64.to_ne_bytes());
    }

    /// Take the oldest unconsumed frame for `stream_id`, with the time it
    /// arrived.
    pub fn take(&self, stream_id: u32) -> Option<(Frame, Instant)> {
        let mut inner = self.inner.lock().unwrap();
        match inner.pending.front() {
            Some((frame, _)) if frame.stream_id == stream_id => inner.pending.pop_front(),
            _ => None,
        }
    }

    /// Whether a frame for `stream_id` is waiting.
    pub fn has_frame(&self, stream_id: u32) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .pending
            .front()
            .is_some_and(|(frame, _)| frame.stream_id == stream_id)
    }
}

#[cfg(test)]
mod tests {
    use std::io::BufReader;

    use super::*;
    use crate::uvc::format::FormatKind;

    const PARAMS: StreamParams = StreamParams {
        kind: FormatKind::Mjpeg,
        width: 640,
        height: 480,
        interval: 333_333,
        max_frame_size: 614_400,
    };

    fn frame(stream_id: u32, byte: u8) -> Frame {
        Frame {
            stream_id,
            pts_us: 0,
            data: vec![byte],
        }
    }

    #[test]
    fn events_frames_and_stale_ids() {
        let hub = SourceHub::default();
        let (daemon_end, app_end) = UnixStream::pair().unwrap();
        let mut events = BufReader::new(app_end);

        // Attaching mid-stream announces the current stream.
        let first = hub.stream_started(PARAMS);
        let source = hub.attach(daemon_end).unwrap();
        assert_eq!(
            SourceEvent::read_from(&mut events).unwrap(),
            SourceEvent::StreamStart {
                stream_id: first,
                params: PARAMS
            }
        );

        // Newest frame wins; taking consumes it.
        hub.submit(source, frame(first, 1));
        hub.submit(source, frame(first, 2));
        assert_eq!(hub.take(first).unwrap().0.data, vec![2]);
        assert!(hub.take(first).is_none());

        // A new stream invalidates frames for the old one.
        hub.stream_stopped();
        assert_eq!(
            SourceEvent::read_from(&mut events).unwrap(),
            SourceEvent::StreamStop { stream_id: first }
        );
        let second = hub.stream_started(PARAMS);
        assert_ne!(first, second);
        SourceEvent::read_from(&mut events).unwrap();
        hub.submit(source, frame(first, 3));
        assert!(hub.take(second).is_none());
        hub.submit(source, frame(second, 4));
        assert_eq!(hub.take(second).unwrap().0.data, vec![4]);

        // A replaced source's frames are ignored.
        let (daemon_end2, _app_end2) = UnixStream::pair().unwrap();
        let replacement = hub.attach(daemon_end2).unwrap();
        hub.submit(source, frame(second, 5));
        assert!(hub.take(second).is_none());
        hub.detach(source);
        assert!(
            hub.is_attached(),
            "stale detach must not remove the new source"
        );
        hub.detach(replacement);
        assert!(!hub.is_attached());
    }

    #[test]
    fn detects_h264_key_frames() {
        // SPS, PPS, IDR slice; then a P slice alone.
        assert!(is_h264_key_frame(&[
            0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0, 0, 1, 0x65
        ]));
        assert!(is_h264_key_frame(&[0, 0, 1, 0x65, 0x88]));
        assert!(!is_h264_key_frame(&[0, 0, 0, 1, 0x41, 0x9a]));
        assert!(!is_h264_key_frame(&[0, 0, 0, 1]));
    }

    #[test]
    fn h264_frames_queue_in_order() {
        const H264: StreamParams = StreamParams {
            kind: FormatKind::H264,
            ..PARAMS
        };
        let idr = |stream_id| Frame {
            stream_id,
            pts_us: 0,
            data: vec![0, 0, 0, 1, 0x65],
        };
        let p = |stream_id, byte| Frame {
            stream_id,
            pts_us: 0,
            data: vec![0, 0, 0, 1, 0x41, byte],
        };

        let hub = SourceHub::default();
        let (daemon_end, app_end) = UnixStream::pair().unwrap();
        let mut events = BufReader::new(app_end);
        let source = hub.attach(daemon_end).unwrap();
        let id = hub.stream_started(H264);
        SourceEvent::read_from(&mut events).unwrap();

        // Frames before the first key frame are useless.
        hub.submit(source, p(id, 0));
        assert!(!hub.has_frame(id));
        hub.submit(source, idr(id));
        hub.submit(source, p(id, 1));
        assert_eq!(hub.take(id).unwrap().0.data, idr(id).data);
        assert_eq!(hub.take(id).unwrap().0.data, p(id, 1).data);
        assert!(hub.take(id).is_none());

        // Overflow drops the queue and asks for a key frame.
        for i in 0..MAX_QUEUED as u8 + 1 {
            hub.submit(source, p(id, i));
        }
        assert!(!hub.has_frame(id));
        assert_eq!(
            SourceEvent::read_from(&mut events).unwrap(),
            SourceEvent::KeyFrame { stream_id: id }
        );
        hub.submit(source, p(id, 9));
        assert!(!hub.has_frame(id));
        hub.submit(source, idr(id));
        assert!(hub.has_frame(id));
    }

    #[test]
    fn dead_source_is_detached_on_write() {
        let hub = SourceHub::default();
        let (daemon_end, app_end) = UnixStream::pair().unwrap();
        hub.attach(daemon_end).unwrap();
        drop(app_end);
        hub.stream_started(PARAMS);
        assert!(!hub.is_attached());
    }
}

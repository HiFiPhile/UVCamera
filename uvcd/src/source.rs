// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! Hand-off between the frame source (the app's attached connection) and
//! the stream thread.
//!
//! At most one source is attached. The stream thread announces each host
//! stream with a fresh id; the source thread stores frames tagged with the
//! current id, keeping only the newest one. Writes to the app use a short
//! timeout so a stuck app can never stall the USB stream.

use std::{
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
    uvc::{BRIGHTNESS_DEF, Control, StreamParams},
};

const WRITE_TIMEOUT: Duration = Duration::from_millis(200);

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
    /// Newest frame for the current stream that the stream thread hasn't
    /// taken yet.
    pending: Option<(Frame, Instant)>,
    /// Brightness the host set, if any.
    brightness: Option<i16>,
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
        if let Some(value) = inner.brightness {
            inner.send(&SourceEvent::Control(Control::Brightness(value)));
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
            inner.pending = None;
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
        inner.pending = None;
        inner.send(&SourceEvent::StreamStart { stream_id, params });
        stream_id
    }

    /// The host changed a control: remember it and tell the source.
    pub fn control_changed(&self, control: Control) {
        let mut inner = self.inner.lock().unwrap();
        match control {
            Control::Brightness(v) => inner.brightness = Some(v),
        }
        inner.send(&SourceEvent::Control(control));
    }

    /// Current control values, for restoring them when the video node is
    /// reopened.
    pub fn controls(&self) -> Vec<Control> {
        let inner = self.inner.lock().unwrap();
        vec![Control::Brightness(
            inner.brightness.unwrap_or(BRIGHTNESS_DEF),
        )]
    }

    /// The host stopped streaming.
    pub fn stream_stopped(&self) {
        let mut inner = self.inner.lock().unwrap();
        if let Some((stream_id, _)) = inner.stream.take() {
            inner.pending = None;
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
        match inner.stream {
            Some((stream_id, _)) if stream_id == frame.stream_id => {
                inner.pending = Some((frame, Instant::now()));
                let _ = rustix::io::write(&self.notify, &1u64.to_ne_bytes());
            }
            _ => debug!("Dropping frame for stream {}", frame.stream_id),
        }
    }

    /// Take the newest unconsumed frame for `stream_id`, with the time it
    /// arrived.
    pub fn take(&self, stream_id: u32) -> Option<(Frame, Instant)> {
        let mut inner = self.inner.lock().unwrap();
        match &inner.pending {
            Some((frame, _)) if frame.stream_id == stream_id => inner.pending.take(),
            _ => None,
        }
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
    fn dead_source_is_detached_on_write() {
        let hub = SourceHub::default();
        let (daemon_end, app_end) = UnixStream::pair().unwrap();
        hub.attach(daemon_end).unwrap();
        drop(app_end);
        hub.stream_started(PARAMS);
        assert!(!hub.is_attached());
    }
}

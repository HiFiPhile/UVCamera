// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! Event loop for one opened gadget video node: answers UVC control
//! requests and feeds frames while the host streams.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use tracing::{debug, info, warn};

use crate::{
    gadget::EndpointConfig,
    pattern::TestPattern,
    uvc::{Protocol, Reply, StreamParams},
    v4l2::{Event, Speed, VideoDevice},
};

const BUFFER_COUNT: u32 = 4;
/// Upper bound on poll() sleeps so stop requests are noticed promptly.
const MAX_WAIT: Duration = Duration::from_millis(100);

/// Why [`Streamer::run`] returned.
#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    /// The stop flag was set.
    Stopped,
    /// The video node went away (gadget unbound or function removed).
    DeviceGone,
}

struct ActiveStream {
    params: StreamParams,
    pattern: TestPattern,
    /// Buffers owned by us (not queued to the driver).
    free: Vec<usize>,
    next_due: Instant,
    sequence: u64,
    dropped: u64,
    started: Instant,
}

pub struct Streamer {
    dev: VideoDevice,
    protocol: Protocol,
    endpoint: EndpointConfig,
    stream: Option<ActiveStream>,
}

impl Streamer {
    pub fn new(dev: VideoDevice, protocol: Protocol, endpoint: EndpointConfig) -> Self {
        Self {
            dev,
            protocol,
            endpoint,
            stream: None,
        }
    }

    pub fn run(&mut self, stop: &AtomicBool) -> Result<Exit> {
        let result = self.event_loop(stop);
        if self.stream.is_some() {
            self.stop_stream();
        }
        result
    }

    fn event_loop(&mut self, stop: &AtomicBool) -> Result<Exit> {
        while !stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            let wait = match &self.stream {
                Some(s) => s.next_due.saturating_duration_since(now).min(MAX_WAIT),
                None => MAX_WAIT,
            };

            // Requesting POLLOUT outside of streaming makes vb2 report POLLERR.
            let mut flags = PollFlags::PRI;
            if self.stream.is_some() {
                flags |= PollFlags::OUT;
            }
            let mut fds = [PollFd::new(&self.dev, flags)];
            let timeout = Timespec::try_from(wait).unwrap();
            match poll(&mut fds, Some(&timeout)) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(e).context("poll() failed"),
            }
            let revents = fds[0].revents();

            if revents.intersects(PollFlags::HUP | PollFlags::NVAL) {
                return Ok(Exit::DeviceGone);
            }
            if revents.contains(PollFlags::PRI) {
                self.drain_events()?;
            }
            if self.stream.is_some() {
                self.reclaim_buffers()?;
                self.feed_frame()?;
            }
            if revents.contains(PollFlags::ERR) && self.stream.is_none() {
                return Ok(Exit::DeviceGone);
            }
        }

        Ok(Exit::Stopped)
    }

    fn drain_events(&mut self) -> Result<()> {
        while let Some(event) = self.dev.dequeue_event()? {
            self.handle_event(event)?;
        }
        Ok(())
    }

    fn handle_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Connect(speed) => {
                let payload = self.endpoint.max_payload(speed);
                info!("Host connected at {speed:?} speed, max payload {payload} bytes");
                if speed < Speed::High {
                    warn!("Full speed link: only tiny frames will stream");
                }
                self.protocol.set_max_payload(payload);
            }
            Event::Disconnect => {
                info!("Host disconnected");
                if self.stream.is_some() {
                    self.stop_stream();
                }
            }
            Event::Setup(req) => match self.protocol.handle_setup(&req) {
                Reply::Data(data) => self.dev.send_response(&data, data.len())?,
                Reply::Receive => self.dev.send_response(&[], req.length.into())?,
                Reply::Stall => {
                    debug!(?req, "Stalling unsupported request");
                    self.dev.send_stall()?;
                }
            },
            Event::Data(data) => {
                if self.protocol.handle_data(&data) {
                    info!("Host committed {:?}", self.protocol.committed());
                }
            }
            Event::StreamOn => {
                if self.stream.is_some() {
                    self.stop_stream();
                }
                self.start_stream()?;
            }
            Event::StreamOff => {
                info!("Host stopped streaming");
                self.stop_stream();
            }
            Event::Other(t) => debug!("Ignoring event {t:#x}"),
        }
        Ok(())
    }

    fn start_stream(&mut self) -> Result<()> {
        let params = self.protocol.committed();
        info!("Host started streaming {params:?}");

        self.dev.set_format(
            params.kind.fourcc(),
            params.width,
            params.height,
            params.max_frame_size,
        )?;
        let count = self.dev.allocate_buffers(BUFFER_COUNT)?;

        let now = Instant::now();
        self.stream = Some(ActiveStream {
            params,
            pattern: TestPattern::new(params.kind, params.width, params.height),
            free: (0..count).collect(),
            next_due: now,
            sequence: 0,
            dropped: 0,
            started: now,
        });

        // Queue the first frame before STREAMON so data flows immediately.
        self.feed_frame()?;
        // This also completes the host's SET_INTERFACE request.
        self.dev.stream_on()
    }

    fn stop_stream(&mut self) {
        let Some(stream) = self.stream.take() else {
            // Still acknowledge a SET_INTERFACE(0) if the kernel deferred it.
            let _ = self.dev.stream_off();
            return;
        };

        let elapsed = stream.started.elapsed().as_secs_f64();
        info!(
            "Stream stopped: {} frames in {elapsed:.1}s ({:.1} fps), {} late",
            stream.sequence,
            stream.sequence as f64 / elapsed.max(f64::EPSILON),
            stream.dropped,
        );

        // STREAMOFF completes the host's SET_INTERFACE(0) and returns all
        // queued buffers.
        if let Err(e) = self.dev.stream_off() {
            warn!("{e:?}");
        }
        if let Err(e) = self.dev.free_buffers() {
            warn!("{e:?}");
        }
    }

    fn reclaim_buffers(&mut self) -> Result<()> {
        let stream = self.stream.as_mut().unwrap();
        while let Some(index) = self.dev.dequeue_buffer()? {
            stream.free.push(index);
        }
        Ok(())
    }

    /// Queue the next frame if one is due and a buffer is free.
    fn feed_frame(&mut self) -> Result<()> {
        let stream = self.stream.as_mut().unwrap();
        let now = Instant::now();
        if now < stream.next_due {
            return Ok(());
        }
        let Some(index) = stream.free.pop() else {
            return Ok(());
        };

        let frame = stream.pattern.frame(stream.sequence)?;
        let buf = self.dev.buffer_mut(index);
        let len = frame.len().min(buf.len());
        if len < frame.len() {
            warn!(
                "Frame of {} bytes truncated to buffer size {}",
                frame.len(),
                buf.len()
            );
        }
        buf[..len].copy_from_slice(&frame[..len]);
        self.dev.queue_buffer(index, len)?;

        let interval = Duration::from_nanos(u64::from(stream.params.interval) * 100);
        stream.sequence += 1;
        stream.next_due += interval;
        // Don't try to catch up after a stall; resynchronize instead.
        if stream.next_due + interval < now {
            stream.dropped += 1;
            stream.next_due = now + interval;
        }

        Ok(())
    }
}

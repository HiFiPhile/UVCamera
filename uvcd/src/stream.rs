// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! Event loop for one opened gadget video node: answers UVC control
//! requests and feeds frames while the host streams.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use tracing::{debug, info, warn};

use crate::{
    gadget::EndpointConfig,
    irq::IrqBoost,
    pattern::TestPattern,
    source::SourceHub,
    uvc::{Protocol, Reply, StreamParams, format::FormatKind},
    v4l2::{Event, Speed, VideoDevice},
};

const BUFFER_COUNT: u32 = 4;
/// Upper bound on poll() sleeps so stop requests are noticed promptly.
const MAX_WAIT: Duration = Duration::from_millis(100);
/// How often streaming statistics are logged.
const STATS_PERIOD: Duration = Duration::from_secs(5);
/// How long the last app frame is repeated when no new one arrives, before
/// falling back to the placeholder pattern.
const REPEAT_LIMIT: Duration = Duration::from_secs(1);

/// Why [`Streamer::run`] returned.
#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    /// The stop flag was set.
    Stopped,
    /// The video node went away (gadget unbound or function removed).
    DeviceGone,
}

/// What the host is doing, for status reporting.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamStatus {
    /// Link speed while a host has the function configured.
    pub host_speed: Option<Speed>,
    /// The stream the host is currently receiving.
    pub streaming: Option<StreamParams>,
    /// Frames sent per second over the last statistics window.
    pub fps: f64,
    /// New frames received from the app per second over the same window.
    pub source_fps: f64,
    /// Frames not fully transferred over the last statistics window.
    pub failed: u64,
}

/// Counters for one [`STATS_PERIOD`] window.
#[derive(Default)]
struct Stats {
    /// Frames handed to the driver.
    queued: u64,
    /// Buffers the driver finished sending.
    completed: u64,
    /// Completed buffers the driver flagged as not fully transferred.
    failed: u64,
    /// New frames taken from the app.
    fresh: u64,
    /// Frames from the placeholder pattern (no app, or app stalled).
    placeholder: u64,
    /// App frames rejected for having the wrong size or format.
    invalid: u64,
    /// Times a frame was due but every buffer was still owned by the driver.
    starved: u64,
    /// Longest time between two buffer completions.
    max_gap: Duration,
    last_completion: Option<Instant>,
    /// Time spent producing and copying frames.
    produce_total: Duration,
    produce_max: Duration,
}

struct ActiveStream {
    params: StreamParams,
    /// Id announced to the frame source for this stream.
    stream_id: u32,
    /// Latest valid app frame and when it arrived.
    last: Option<(Vec<u8>, Instant)>,
    /// Placeholder when no app frames are available.
    pattern: TestPattern,
    /// Buffers owned by us (not queued to the driver).
    free: Vec<usize>,
    next_due: Instant,
    sequence: u64,
    dropped: u64,
    started: Instant,
    stats: Stats,
    stats_since: Instant,
}

pub struct Streamer {
    dev: VideoDevice,
    protocol: Protocol,
    endpoint: EndpointConfig,
    irq_boost: Option<IrqBoost>,
    stream: Option<ActiveStream>,
    status: Arc<Mutex<StreamStatus>>,
    source: Arc<SourceHub>,
}

impl Streamer {
    pub fn new(
        dev: VideoDevice,
        protocol: Protocol,
        endpoint: EndpointConfig,
        irq_boost: Option<IrqBoost>,
        status: Arc<Mutex<StreamStatus>>,
        source: Arc<SourceHub>,
    ) -> Self {
        Self {
            dev,
            protocol,
            endpoint,
            irq_boost,
            stream: None,
            status,
            source,
        }
    }

    fn update_status(&self, f: impl FnOnce(&mut StreamStatus)) {
        f(&mut self.status.lock().unwrap());
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
                self.report_stats();
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
                self.update_status(|s| s.host_speed = Some(speed));
            }
            Event::Disconnect => {
                info!("Host disconnected");
                if self.stream.is_some() {
                    self.stop_stream();
                }
                self.update_status(|s| *s = StreamStatus::default());
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

        if let Some(boost) = &mut self.irq_boost {
            boost.apply();
        }

        self.dev.set_format(
            params.kind.fourcc(),
            params.width,
            params.height,
            params.max_frame_size,
        )?;
        if !self.dev.set_frame_interval(params.interval)? {
            // Pre-6.13 f_uvc sizes isochronous requests from the endpoint
            // configuration alone and needs no interval.
            debug!("Kernel does not support VIDIOC_S_PARM");
        }
        let count = self.dev.allocate_buffers(BUFFER_COUNT)?;

        let stream_id = self.source.stream_started(params);
        let now = Instant::now();
        self.stream = Some(ActiveStream {
            params,
            stream_id,
            last: None,
            pattern: TestPattern::new(params.kind, params.width, params.height),
            free: (0..count).collect(),
            next_due: now,
            sequence: 0,
            dropped: 0,
            started: now,
            stats: Stats::default(),
            stats_since: now,
        });

        self.update_status(|s| {
            s.streaming = Some(params);
            s.fps = 0.0;
            s.failed = 0;
        });

        // Queue the first frame before STREAMON so data flows immediately.
        self.feed_frame()?;
        // This also completes the host's SET_INTERFACE request.
        self.dev.stream_on()
    }

    fn stop_stream(&mut self) {
        if let Some(boost) = &mut self.irq_boost {
            boost.restore();
        }
        self.source.stream_stopped();

        self.update_status(|s| {
            s.streaming = None;
            s.fps = 0.0;
            s.failed = 0;
        });

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
        while let Some((index, failed)) = self.dev.dequeue_buffer()? {
            stream.free.push(index);

            let now = Instant::now();
            let stats = &mut stream.stats;
            if let Some(last) = stats.last_completion {
                stats.max_gap = stats.max_gap.max(now - last);
            }
            stats.last_completion = Some(now);
            stats.completed += 1;
            stats.failed += u64::from(failed);
        }
        Ok(())
    }

    fn report_stats(&mut self) {
        let stream = self.stream.as_mut().unwrap();
        let elapsed = stream.stats_since.elapsed();
        if elapsed < STATS_PERIOD {
            return;
        }

        let s = std::mem::take(&mut stream.stats);
        stream.stats_since = Instant::now();
        let fps = s.completed as f64 / elapsed.as_secs_f64();
        let source_fps = s.fresh as f64 / elapsed.as_secs_f64();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        info!(
            "Stats: sent {fps:.1} fps, app {source_fps:.1} fps, placeholder {}, invalid {}, failed {}, queued {}, starved {}, max gap {:.1} ms, produce avg {:.1} ms / max {:.1} ms",
            s.placeholder,
            s.invalid,
            s.failed,
            s.queued,
            s.starved,
            ms(s.max_gap),
            ms(s.produce_total) / s.queued.max(1) as f64,
            ms(s.produce_max),
        );
        self.update_status(|status| {
            status.fps = fps;
            status.source_fps = source_fps;
            status.failed = s.failed;
        });
    }

    /// Queue the next frame if one is due and a buffer is free.
    fn feed_frame(&mut self) -> Result<()> {
        let stream = self.stream.as_mut().unwrap();
        let now = Instant::now();
        if now < stream.next_due {
            return Ok(());
        }
        let Some(index) = stream.free.pop() else {
            stream.stats.starved += 1;
            return Ok(());
        };

        if let Some((frame, _)) = self.source.take(stream.stream_id) {
            if frame_is_valid(&stream.params, &frame.data) {
                stream.stats.fresh += 1;
                stream.last = Some((frame.data, now));
            } else {
                stream.stats.invalid += 1;
            }
        }
        let frame = match &stream.last {
            Some((data, at)) if now.duration_since(*at) < REPEAT_LIMIT => data.as_slice(),
            _ => {
                stream.stats.placeholder += 1;
                stream.pattern.frame(stream.sequence)?
            }
        };
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

        let produce = now.elapsed();
        stream.stats.queued += 1;
        stream.stats.produce_total += produce;
        stream.stats.produce_max = stream.stats.produce_max.max(produce);

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

/// Whether an app frame matches the stream's format: YUYV frames are exactly
/// two bytes per pixel, MJPEG frames start with a JPEG SOI marker and fit
/// the negotiated maximum.
fn frame_is_valid(params: &StreamParams, data: &[u8]) -> bool {
    match params.kind {
        FormatKind::Yuyv => {
            data.len() == usize::from(params.width) * usize::from(params.height) * 2
        }
        FormatKind::Mjpeg => {
            data.starts_with(&[0xff, 0xd8]) && data.len() <= params.max_frame_size as usize
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_app_frames() {
        let yuyv = StreamParams {
            kind: FormatKind::Yuyv,
            width: 4,
            height: 2,
            interval: 333_333,
            max_frame_size: 16,
        };
        assert!(frame_is_valid(&yuyv, &[0; 16]));
        assert!(!frame_is_valid(&yuyv, &[0; 15]));

        let mjpeg = StreamParams {
            kind: FormatKind::Mjpeg,
            max_frame_size: 4,
            ..yuyv
        };
        assert!(frame_is_valid(&mjpeg, &[0xff, 0xd8, 0, 0]));
        assert!(!frame_is_valid(&mjpeg, &[0xff, 0xd8, 0, 0, 0]));
        assert!(!frame_is_valid(&mjpeg, &[0, 0]));
    }
}

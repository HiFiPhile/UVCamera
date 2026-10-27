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
    util::monotonic_us,
    uvc::{DataOutcome, Protocol, Reply, StreamParams, format::FormatKind},
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
/// Minimum spacing between queued frames, in frame intervals. Without it, a
/// repeat followed by a late fresh frame sends two frames back to back and
/// the host sees more frames per second than negotiated.
const MIN_GAP_INTERVALS: f64 = 0.5;

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
    /// Frames handed to the driver, and their bytes.
    queued: u64,
    queued_bytes: u64,
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
    /// The last app frame sent again because no new one arrived in time.
    repeated: u64,
    /// Capture (sensor timestamp) to USB queue, for fresh frames.
    latency_total: Duration,
    latency_max: Duration,
    /// Part of the latency spent waiting in the daemon for a free buffer.
    wait_total: Duration,
    /// Times a frame was due but every buffer was still owned by the driver.
    starved: u64,
    /// Longest time between two buffer completions.
    max_gap: Duration,
    last_completion: Option<Instant>,
    /// Time spent producing and copying frames.
    produce_total: Duration,
    produce_max: Duration,
}

struct AppFrame {
    data: Vec<u8>,
    /// When the daemon received it.
    arrived: Instant,
    /// Capture time from the app (CLOCK_MONOTONIC, µs).
    pts_us: u64,
    /// Not yet queued to the driver.
    unsent: bool,
}

/// When to repeat the last app frame. Fresh frames are sent as they arrive;
/// repeats fill in between. The app's frame period is estimated from
/// arrivals: when it spans several stream intervals (a camera held to 30
/// fps on a 60 fps stream), each frame is repeated on the stream's interval
/// grid, so the host gets an even cadence at the negotiated rate. Past the
/// expected next frame, one more interval of slack absorbs encoder jitter
/// before a stall repeat: with only half an interval, every late frame
/// caused a repeat followed by the fresh frame, i.e. more frames per second
/// than negotiated.
struct Pacer {
    interval: Duration,
    /// Average time between app frames, once two have arrived.
    source_gap: Option<Duration>,
    last_arrival: Option<Instant>,
    /// When the last fresh frame was queued.
    fresh_sent: Option<Instant>,
    /// Repeats queued since then.
    repeats: u32,
}

impl Pacer {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            source_gap: None,
            last_arrival: None,
            fresh_sent: None,
            repeats: 0,
        }
    }

    /// Note an app frame's arrival.
    fn on_arrival(&mut self, at: Instant) {
        if let Some(prev) = self.last_arrival {
            // Stalls (camera switches) shouldn't dominate the average.
            let gap = at.duration_since(prev).min(self.interval * 4);
            self.source_gap = Some(match self.source_gap {
                Some(avg) => (avg * 7 + gap) / 8,
                None => gap,
            });
        }
        self.last_arrival = Some(at);
    }

    /// Stream intervals per app frame, e.g. 2 for 30 fps on a 60 fps
    /// stream; 1 when the app keeps up or runs only somewhat slower.
    fn ratio(&self) -> u32 {
        let gap = self.source_gap.unwrap_or(self.interval);
        // A little slack so a camera at exactly half rate counts as 2.
        (gap.as_secs_f64() / self.interval.as_secs_f64() + 0.1).floor().max(1.0) as u32
    }

    /// When the next repeat is due, after queuing a frame at `now`.
    fn next_due(&mut self, now: Instant, fresh: bool) -> Instant {
        if fresh {
            self.fresh_sent = Some(now);
            self.repeats = 0;
        } else {
            self.repeats += 1;
        }
        let base = self.fresh_sent.unwrap_or(now);
        let ratio = self.ratio();
        let slot = self.repeats + 1;
        if slot < ratio {
            // Within the app's frame period: the next grid slot.
            base + self.interval * slot
        } else if slot == ratio {
            // The next fresh frame is due; allow an interval of slack.
            base + self.interval * (ratio + 1)
        } else {
            // Stalled: repeat every interval.
            now + self.interval
        }
    }
}

struct ActiveStream {
    params: StreamParams,
    /// Id announced to the frame source for this stream.
    stream_id: u32,
    /// Latest valid app frame.
    last: Option<AppFrame>,
    /// Placeholder when no app frames are available.
    pattern: TestPattern,
    /// Buffers owned by us (not queued to the driver).
    free: Vec<usize>,
    next_due: Instant,
    /// When the last frame was queued.
    last_sent: Option<Instant>,
    pacer: Pacer,
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
            let source = self.source.clone();
            let mut fds = [
                PollFd::new(&self.dev, flags),
                PollFd::from_borrowed_fd(source.notify_fd(), PollFlags::IN),
            ];
            // Only wake for app frames while streaming.
            let nfds = if self.stream.is_some() { 2 } else { 1 };
            let timeout = Timespec::try_from(wait).unwrap();
            match poll(&mut fds[..nfds], Some(&timeout)) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(e).context("poll() failed"),
            }
            let revents = fds[0].revents();
            if fds[1].revents().contains(PollFlags::IN) {
                source.clear_notify();
            }

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
            Event::Data(data) => match self.protocol.handle_data(&data) {
                DataOutcome::None => {}
                DataOutcome::Committed => {
                    info!("Host committed {:?}", self.protocol.committed());
                }
                DataOutcome::ControlChanged(control) => {
                    info!("Host set {control:?}");
                    self.source.control_changed(control);
                }
            },
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
            last_sent: None,
            pacer: Pacer::new(Duration::from_nanos(u64::from(params.interval) * 100)),
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
            "Stats: sent {fps:.1} fps, app {source_fps:.1} fps, latency avg {:.1} ms / max {:.1} ms (daemon wait avg {:.1} ms), repeated {}, placeholder {}, invalid {}, failed {}, queued {} (avg {} KiB), starved {}, max gap {:.1} ms, produce avg {:.1} ms / max {:.1} ms",
            ms(s.latency_total) / s.fresh.max(1) as f64,
            ms(s.latency_max),
            ms(s.wait_total) / s.fresh.max(1) as f64,
            s.repeated,
            s.placeholder,
            s.invalid,
            s.failed,
            s.queued,
            s.queued_bytes / s.queued.max(1) / 1024,
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

    /// Queue the next frame if one is ready and a buffer is free: a fresh app
    /// frame as soon as it arrives, otherwise, once due, a repeat of the last
    /// app frame or the placeholder pattern.
    fn feed_frame(&mut self) -> Result<()> {
        let stream = self.stream.as_mut().unwrap();
        let now = Instant::now();

        if let Some((frame, arrived)) = self.source.take(stream.stream_id) {
            if frame_is_valid(&stream.params, &frame.data) {
                stream.stats.fresh += 1;
                stream.pacer.on_arrival(arrived);
                stream.last = Some(AppFrame {
                    data: frame.data,
                    arrived,
                    pts_us: frame.pts_us,
                    unsent: true,
                });
            } else {
                stream.stats.invalid += 1;
            }
        }

        let fresh = stream.last.as_ref().is_some_and(|f| f.unsent);
        if !fresh && now < stream.next_due {
            return Ok(());
        }
        let interval = Duration::from_nanos(u64::from(stream.params.interval) * 100);
        if let Some(last) = stream.last_sent {
            let earliest = last + interval.mul_f64(MIN_GAP_INTERVALS);
            if now < earliest {
                // Try again then; the event loop's timeout covers it.
                stream.next_due = stream.next_due.min(earliest);
                return Ok(());
            }
        }
        let Some(index) = stream.free.pop() else {
            stream.stats.starved += 1;
            return Ok(());
        };

        let frame = match &mut stream.last {
            Some(f) if f.unsent => {
                f.unsent = false;
                let capture_us = monotonic_us().saturating_sub(f.pts_us);
                // Ignore timestamps from a different clock or the future.
                if capture_us < 10_000_000 {
                    let latency = Duration::from_micros(capture_us);
                    stream.stats.latency_total += latency;
                    stream.stats.latency_max = stream.stats.latency_max.max(latency);
                }
                stream.stats.wait_total += now.duration_since(f.arrived);
                f.data.as_slice()
            }
            Some(f) if now.duration_since(f.arrived) < REPEAT_LIMIT => {
                stream.stats.repeated += 1;
                f.data.as_slice()
            }
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
        stream.stats.queued_bytes += len as u64;
        stream.stats.produce_total += produce;
        stream.stats.produce_max = stream.stats.produce_max.max(produce);

        stream.last_sent = Some(now);
        stream.sequence += 1;
        stream.next_due = stream.pacer.next_due(now, fresh);
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

    /// Times (ms) at which frames are queued, given app frame arrival times
    /// (ms), following feed_frame's rules: fresh frames at once (spaced by
    /// MIN_GAP_INTERVALS), repeats when due.
    fn simulate(interval_ms: u64, arrivals: &[u64], until_ms: u64) -> Vec<(u64, bool)> {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let interval = Duration::from_millis(interval_ms);
        let mut pacer = Pacer::new(interval);
        let mut next_due = at(0);
        let mut last_sent: Option<Instant> = None;
        let mut pending = false;
        let mut have_frame = false;
        let mut arrivals = arrivals.iter().peekable();
        let mut sent = Vec::new();
        for ms in 0..until_ms {
            let now = at(ms);
            if arrivals.peek().is_some_and(|&&a| a == ms) {
                arrivals.next();
                pacer.on_arrival(now);
                pending = true;
                have_frame = true;
            }
            if !have_frame || (!pending && now < next_due) {
                continue;
            }
            if last_sent.is_some_and(|l| now < l + interval.mul_f64(MIN_GAP_INTERVALS)) {
                continue;
            }
            next_due = pacer.next_due(now, pending);
            sent.push((ms, pending));
            pending = false;
            last_sent = Some(now);
        }
        sent
    }

    #[test]
    fn half_rate_app_frames_are_repeated_once_on_the_grid() {
        // 30 fps app (33-34 ms apart, jittery) on a 60 fps (17 ms) stream.
        let arrivals: Vec<u64> = (0..60).map(|i| i * 100 / 3 + [0, 4, 1, 3][i as usize % 4]).collect();
        let sent = simulate(17, &arrivals, 2000);
        let fresh = sent.iter().filter(|(_, f)| *f).count();
        let repeats = sent.len() - fresh;
        // After the rate estimate settles, one repeat per fresh frame.
        let late: Vec<_> = sent.iter().filter(|(ms, _)| *ms > 500).collect();
        let late_repeats = late.iter().filter(|(_, f)| !*f).count();
        assert_eq!(fresh, 60);
        assert!(repeats >= 50, "{repeats} repeats");
        assert!(late_repeats * 2 >= late.len() - 1, "{late:?}");
        // Then never more than the negotiated rate over any 100 ms.
        for w in late.windows(7) {
            assert!(w[6].0 - w[0].0 >= 90, "{w:?}");
        }
        // Repeats land midway between fresh frames.
        for w in late.windows(2).filter(|w| w[0].1 && !w[1].1) {
            assert!((14..=20).contains(&(w[1].0 - w[0].0)), "{w:?}");
        }
    }

    #[test]
    fn full_rate_app_frames_are_not_repeated() {
        // 58.5 fps app with jitter on a 60 fps stream.
        let arrivals: Vec<u64> = (0..100).map(|i| i * 1000 / 58 + [0, 3, 1][i as usize % 3]).collect();
        let sent = simulate(17, &arrivals, 1700);
        assert!(sent.iter().all(|(_, fresh)| *fresh), "{sent:?}");
    }

    #[test]
    fn stalls_repeat_every_interval() {
        let arrivals: Vec<u64> = (0..10).map(|i| i * 33).collect();
        let sent = simulate(33, &arrivals, 600);
        // Last fresh frame at 297 ms; repeats from 2 intervals later.
        let repeats: Vec<u64> = sent.iter().filter(|(_, f)| !*f).map(|(ms, _)| *ms).collect();
        assert_eq!(repeats.first(), Some(&363));
        assert!(repeats.windows(2).all(|w| w[1] - w[0] == 33), "{repeats:?}");
    }

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

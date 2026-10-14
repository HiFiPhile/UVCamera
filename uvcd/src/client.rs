// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! `uvcd client`: talk to a running daemon from a root shell. Root bypasses
//! the UID authorization; unprivileged shells are rejected.

use std::{
    io::{self, BufReader},
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use byteorder::{ReadBytesExt, WriteBytesExt};
use clap::{Parser, Subcommand};

use crate::{
    daemon::socket_addr,
    gadget_cli::{StreamOpts, build_specs},
    message::{self, EnableRequest, Frame, Request, Response, SourceEvent},
    pattern::TestPattern,
    util::monotonic_us,
    uvc::{StreamParams, format::FormatKind},
};

pub struct Client {
    stream: UnixStream,
}

impl Client {
    pub fn connect() -> Result<Self> {
        let mut stream = UnixStream::connect_addr(&socket_addr())
            .with_context(|| format!("Failed to connect to @{}", message::SOCKET_NAME))?;

        stream.write_u8(message::PROTOCOL_VERSION)?;
        match stream.read_u8() {
            Ok(1) => {}
            Ok(_) => bail!(
                "Daemon rejected protocol version {}",
                message::PROTOCOL_VERSION
            ),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                bail!("Daemon closed the connection (not authorized, or SELinux check failed)")
            }
            Err(e) => return Err(e).context("Failed to negotiate protocol"),
        }

        Ok(Self { stream })
    }

    pub fn request(&mut self, request: &Request) -> Result<Response> {
        request
            .write_to(&mut self.stream)
            .context("Failed to send request")?;
        Response::read_from(&mut self.stream).context("Failed to receive response")
    }
}

/// Attach as the frame source and feed a reversed test pattern in whatever
/// format the host commits, until the daemon goes away.
fn run_source(mut client: Client) -> Result<()> {
    expect_ok(client.request(&Request::AttachSource)?)?;
    println!("Attached as frame source");

    let mut writer = client.stream;
    let mut reader = BufReader::new(writer.try_clone()?);
    let current: Arc<Mutex<Option<(u32, StreamParams)>>> = Arc::default();

    let events = {
        let current = current.clone();
        thread::spawn(move || {
            loop {
                match SourceEvent::read_from(&mut reader) {
                    Ok(event) => {
                        println!("{event:?}");
                        match event {
                            SourceEvent::StreamStart { stream_id, params } => {
                                *current.lock().unwrap() = Some((stream_id, params));
                            }
                            SourceEvent::StreamStop { .. } => *current.lock().unwrap() = None,
                            // A test pattern has nothing to adjust.
                            SourceEvent::Control(_) => {}
                        }
                    }
                    Err(e) => {
                        println!("Source channel closed: {e}");
                        return;
                    }
                }
            }
        })
    };

    let mut pattern: Option<(u32, TestPattern)> = None;
    let mut sequence = 0;
    let mut next = Instant::now();

    while !events.is_finished() {
        let Some((stream_id, params)) = *current.lock().unwrap() else {
            thread::sleep(Duration::from_millis(50));
            continue;
        };

        if pattern.as_ref().is_none_or(|(id, _)| *id != stream_id) {
            pattern = Some((
                stream_id,
                TestPattern::new(params.kind, params.width, params.height).reversed(),
            ));
            sequence = 0;
            next = Instant::now();
        }
        let (_, p) = pattern.as_mut().unwrap();

        Frame {
            stream_id,
            pts_us: monotonic_us(),
            data: p.frame(sequence)?.to_vec(),
        }
        .write_to(&mut writer)
        .context("Failed to send frame")?;
        sequence += 1;

        next += Duration::from_nanos(u64::from(params.interval) * 100);
        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        } else {
            next = now;
        }
    }

    Ok(())
}

fn expect_ok(response: Response) -> Result<()> {
    match response {
        Response::Ok => Ok(()),
        Response::Error(e) => bail!("Daemon error: {e}"),
        r => bail!("Unexpected response: {r:?}"),
    }
}

pub fn subcommand_client(cli: &ClientCli) -> Result<()> {
    let mut client = Client::connect()?;

    match &cli.command {
        ClientCommand::Status => match client.request(&Request::GetStatus)? {
            Response::Status(s) => {
                println!("Enabled: {}", s.enabled);
                match s.stream.host_speed {
                    Some(speed) => println!("Host: connected at {speed:?} speed"),
                    None => println!("Host: not connected"),
                }
                match s.stream.streaming {
                    Some(p) => {
                        let fps = 10_000_000.0 / f64::from(p.interval);
                        let kind = match p.kind {
                            FormatKind::Mjpeg => "MJPEG",
                            FormatKind::Yuyv => "YUYV",
                        };
                        println!("Streaming: {kind} {}x{} @ {fps:.0} fps", p.width, p.height);
                        println!(
                            "Last 5 s: {:.1} fps sent, {:.1} fps from the app, {} failed",
                            s.stream.fps, s.stream.source_fps, s.stream.failed
                        );
                    }
                    None => println!("Streaming: no"),
                }
                Ok(())
            }
            r => expect_ok(r),
        },
        ClientCommand::Enable(opts) => {
            expect_ok(client.request(&Request::Enable(EnableRequest {
                maxburst: opts.maxburst as u8,
                formats: build_specs(&opts.formats, &opts.mjpeg_fps),
                controls: Default::default(),
            }))?)
        }
        ClientCommand::Disable => expect_ok(client.request(&Request::Disable)?),
        ClientCommand::Source => run_source(client),
    }
}

#[derive(Debug, Subcommand)]
enum ClientCommand {
    /// Show whether the webcam is enabled and what the host is doing.
    Status,
    /// Enable the webcam (test pattern).
    Enable(StreamOpts),
    /// Disable the webcam and restore the USB configuration.
    Disable,
    /// Act as the frame source, feeding a reversed test pattern. For testing
    /// the source path without the app.
    Source,
}

/// Talk to the running daemon.
#[derive(Debug, Parser)]
pub struct ClientCli {
    #[command(subcommand)]
    command: ClientCommand,
}

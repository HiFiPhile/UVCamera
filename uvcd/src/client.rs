// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! `uvcd client`: talk to a running daemon from a root shell. Root bypasses
//! the UID authorization; unprivileged shells are rejected.

use std::{io, os::unix::net::UnixStream};

use anyhow::{Context, Result, bail};
use byteorder::{ReadBytesExt, WriteBytesExt};
use clap::{Parser, Subcommand};

use crate::{
    daemon::socket_addr,
    gadget_cli::{StreamOpts, build_specs},
    message::{self, EnableRequest, Request, Response},
    uvc::format::FormatKind,
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
                            "Last 5 s: {:.1} fps sent, {} failed",
                            s.stream.fps, s.stream.failed
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
            }))?)
        }
        ClientCommand::Disable => expect_ok(client.request(&Request::Disable)?),
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
}

/// Talk to the running daemon.
#[derive(Debug, Parser)]
pub struct ClientCli {
    #[command(subcommand)]
    command: ClientCommand,
}

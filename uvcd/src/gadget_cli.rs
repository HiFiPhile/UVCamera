// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! `uvcd gadget up|down`: run the UVC gadget with a test pattern, without
//! the app. Phase 1 bring-up tool; requires root.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use tracing::{info, warn};

use crate::{
    gadget::{BULK_MAX_PAYLOAD, EndpointConfig, FUNCTION_UVC, FUNCTION_UVCX, Gadget, Transfer},
    session::{Original, Session, SessionConfig, teardown},
    source::SourceHub,
    uvc::{
        controls::ControlCaps,
        format::{FormatKind, FormatSpec},
    },
};

/// Default bulk payload: whole frames for most MJPEG sizes.
const DEFAULT_BULK_PAYLOAD: u32 = 512 * 1024;

/// What `up` changed, so `down` can undo it after a crash.
#[derive(Debug, PartialEq, Eq)]
struct State {
    pid: u32,
    original: Original,
}

impl State {
    fn load(path: &Path) -> Result<Option<Self>> {
        let data = match fs::read_to_string(path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("Failed to read {path:?}")),
        };
        Self::parse(&data)
            .map(Some)
            .with_context(|| format!("Invalid state file: {path:?}"))
    }

    fn parse(data: &str) -> Result<Self> {
        let mut pid = None;
        let mut udc = None;
        let mut class = None;
        for line in data.lines() {
            match line.split_once('=') {
                Some(("pid", v)) => pid = Some(v.parse()?),
                Some(("udc", v)) => udc = Some(v.to_owned()),
                Some(("device_class", v)) => {
                    let bytes = v
                        .split(',')
                        .map(|b| u8::from_str_radix(b, 16))
                        .collect::<Result<Vec<_>, _>>()?;
                    class = Some(<[u8; 3]>::try_from(bytes).map_err(|b| anyhow::anyhow!("{b:?}"))?);
                }
                _ => {}
            }
        }
        match (pid, udc, class) {
            (Some(pid), Some(udc), Some(device_class)) => Ok(Self {
                pid,
                original: Original { udc, device_class },
            }),
            _ => bail!("Missing fields"),
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("Failed to create {parent:?}"))?;
        }
        let [a, b, c] = self.original.device_class;
        fs::write(
            path,
            format!(
                "pid={}\nudc={}\ndevice_class={a:02x},{b:02x},{c:02x}\n",
                self.pid, self.original.udc
            ),
        )
        .with_context(|| format!("Failed to write {path:?}"))
    }
}

fn process_alive(pid: u32) -> bool {
    // SAFETY: Signal 0 only checks for existence.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Stop a running `up` instance and clean up anything it left behind.
fn down(gadget: &Gadget, state_file: &Path) -> Result<()> {
    let state = State::load(state_file)?;

    if let Some(state) = &state
        && state.pid != std::process::id()
        && process_alive(state.pid)
    {
        info!("Stopping uvcd (pid {})", state.pid);
        // SAFETY: Sending a signal has no memory safety implications.
        unsafe { libc::kill(state.pid as libc::pid_t, libc::SIGTERM) };

        let deadline = Instant::now() + Duration::from_secs(5);
        while process_alive(state.pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }
        if process_alive(state.pid) {
            bail!("uvcd (pid {}) did not exit", state.pid);
        }
    }

    teardown(gadget, state.as_ref().map(|s| &s.original))?;

    match fs::remove_file(state_file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to remove {state_file:?}")),
    }
}

/// Format list for `--format` / `--mjpeg-fps`.
pub fn build_specs(formats: &[FormatKind], mjpeg_fps: &[u32]) -> Vec<FormatSpec> {
    formats
        .iter()
        .map(|&kind| match kind {
            FormatKind::Mjpeg | FormatKind::H264 => {
                FormatSpec::default_for(kind).with_fps(mjpeg_fps)
            }
            FormatKind::Yuyv => FormatSpec::default_for(kind),
        })
        .collect()
}

fn up(gadget: &Gadget, cli: &UpCli, state_file: &Path) -> Result<()> {
    if !rustix::process::getuid().is_root() {
        warn!("Not running as root; configfs changes will likely fail");
    }
    let maxpacket = cli.maxpacket.unwrap_or(match cli.transfer {
        Transfer::Isoc => 3072,
        Transfer::Bulk => DEFAULT_BULK_PAYLOAD,
    });
    if cli.transfer == Transfer::Isoc && maxpacket > 3072 {
        bail!("Isochronous max packet size is at most 3072");
    }

    if gadget.function_exists() || gadget.is_linked() || state_file.exists() {
        warn!("Cleaning up state from a previous run");
        down(gadget, state_file)?;
    }

    let stop = Arc::new(AtomicBool::new(false));
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        signal_hook::flag::register(signal, stop.clone())
            .with_context(|| format!("Failed to register handler for signal {signal}"))?;
    }

    let session = Session::start(
        gadget.clone(),
        SessionConfig {
            specs: build_specs(&cli.stream.formats, &cli.stream.mjpeg_fps),
            // The test pattern has nothing to control beyond brightness.
            controls: ControlCaps::default(),
            endpoint: EndpointConfig {
                transfer: cli.transfer,
                maxpacket,
                maxburst: cli.stream.maxburst,
            },
            udc: cli.udc.clone(),
            irq_boost: !cli.no_irq_boost,
        },
        // No app attaches here, so the host always sees the test pattern.
        Arc::new(SourceHub::default()),
    )?;

    let state = State {
        pid: std::process::id(),
        original: session.original().clone(),
    };
    if let Err(e) = state.save(state_file) {
        warn!("{e:?}");
    }

    while !stop.load(Ordering::Relaxed) && !session.is_finished() {
        thread::sleep(Duration::from_millis(100));
    }

    let result = session.stop();
    let _ = fs::remove_file(state_file);
    result
}

pub fn subcommand_gadget(cli: &GadgetCli) -> Result<()> {
    let gadget =
        Gadget::new(&cli.common.gadget, &cli.common.config)?.with_function(&cli.common.function);

    match &cli.command {
        GadgetCommand::Up(c) => up(&gadget, c, &cli.common.state_file),
        GadgetCommand::Down => down(&gadget, &cli.common.state_file),
    }
}

#[derive(Debug, Args)]
struct CommonOpts {
    /// Root of the configfs USB gadget.
    #[arg(
        long,
        value_name = "PATH",
        default_value = "/config/usb_gadget/g1",
        global = true
    )]
    gadget: PathBuf,

    /// Gadget configuration to add the UVC function to.
    #[arg(long, value_name = "NAME", default_value = "b.1", global = true)]
    config: String,

    /// UVC function driver: the kernel's built-in one, or the usb_f_uvcx
    /// module (frame-based formats, bulk streaming), which must be loaded.
    #[arg(
        long,
        value_name = "NAME",
        default_value = FUNCTION_UVC,
        value_parser = [FUNCTION_UVC, FUNCTION_UVCX],
        global = true
    )]
    function: String,

    /// Records what `up` changed so `down` can restore it.
    #[arg(
        long,
        value_name = "PATH",
        default_value = "/data/local/tmp/uvcamera/gadget.state",
        global = true
    )]
    state_file: PathBuf,
}

/// What to advertise to the host. Shared with `uvcd client enable`.
#[derive(Debug, Args)]
pub struct StreamOpts {
    /// Formats to advertise, in order of preference.
    #[arg(long = "format", value_name = "FORMAT", default_values = ["mjpeg", "yuyv"])]
    pub formats: Vec<FormatKind>,

    /// Frame rates offered for every MJPEG frame size. The highest is the
    /// default.
    #[arg(
        long = "mjpeg-fps",
        value_name = "FPS",
        default_values_t = [30, 15],
        value_parser = clap::value_parser!(u32).range(1..=120)
    )]
    pub mjpeg_fps: Vec<u32>,

    /// Isochronous max burst for SuperSpeed (0-15).
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u32).range(0..=15))]
    pub maxburst: u32,
}

#[derive(Debug, Parser)]
struct UpCli {
    #[command(flatten)]
    stream: StreamOpts,

    /// USB device controller to bind (default: the gadget's current one).
    #[arg(long, value_name = "NAME")]
    udc: Option<String>,

    /// Streaming endpoint type. Bulk needs `--function uvcx`.
    #[arg(long, value_enum, default_value_t = Transfer::Isoc)]
    transfer: Transfer,

    /// Isochronous: max packet size (1-3072, default 3072). Bulk: payload
    /// size (default 512 KiB).
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=i64::from(BULK_MAX_PAYLOAD)))]
    maxpacket: Option<u32>,

    /// Don't raise the USB controller's IRQ thread to real-time priority
    /// while streaming.
    #[arg(long)]
    no_irq_boost: bool,
}

#[derive(Debug, Subcommand)]
enum GadgetCommand {
    /// Add the UVC function and stream a test pattern until interrupted.
    Up(UpCli),
    /// Stop a running `up` and restore the original USB configuration.
    Down,
}

/// Run the UVC gadget with a test pattern (no app needed).
#[derive(Debug, Parser)]
pub struct GadgetCli {
    #[command(flatten)]
    common: CommonOpts,

    #[command(subcommand)]
    command: GadgetCommand,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_roundtrip() {
        let dir = std::env::temp_dir().join(format!("uvcd-test-{}", std::process::id()));
        let path = dir.join("gadget.state");
        let state = State {
            pid: 1234,
            original: Original {
                udc: "a600000.dwc3".into(),
                device_class: [0xef, 0x02, 0x01],
            },
        };
        state.save(&path).unwrap();
        assert_eq!(State::load(&path).unwrap(), Some(state));
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(State::load(&path).unwrap(), None);
    }
}

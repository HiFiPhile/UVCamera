// SPDX-FileCopyrightText: 2024-2025 Andrew Gunnerson
// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only
// Based on msd-tool's daemon from MSD.

//! The daemon the app talks to. It runs in the `uvcamera_daemon` SELinux
//! domain as the system user with only CAP_CHOWN (configfs creates nodes as
//! root) and CAP_SYS_NICE (USB IRQ thread priority), plus the readproc group
//! to see kernel threads in /proc. It owns at most one gadget [`Session`].
//!
//! A root `app_process` supervisor started it and answers, over inherited
//! pipes, whether a peer UID belongs to the UVCamera package signed with the
//! pinned certificate. Every request is re-authorized. SELinux must be
//! enforcing with our policy loaded. Protocol violations close the
//! connection; valid requests that fail get an error response.

use std::{
    fs::File,
    io,
    os::unix::net::{SocketAddr, UnixListener, UnixStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

#[cfg(target_os = "android")]
use std::os::android::net::SocketAddrExt;
#[cfg(target_os = "linux")]
use std::os::linux::net::SocketAddrExt;

use anyhow::{Context, Result, bail};
use byteorder::{ReadBytesExt, WriteBytesExt};
use clap::Parser;
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fs::{Gid, Uid},
    thread::{CapabilitySet, CapabilitySets},
};
use tracing::{debug, error, info, info_span, trace, warn};

use crate::{
    auth::Authenticator,
    gadget::{EndpointConfig, Gadget, Transfer},
    message::{self, EnableRequest, Frame, Request, Response, Status},
    session::{Session, SessionConfig, teardown},
    source::SourceHub,
    stream::StreamStatus,
};

const SELINUX_ENFORCE: &str = "/sys/fs/selinux/enforce";
const SELINUX_MAGIC: u32 = 0xf97c_ff8c;
const SYSTEM_UID: u32 = 1000;
/// AID_READPROC: /proc is mounted with `hidepid=invisible,gid=readproc`, and
/// the IRQ boost needs to see kernel threads.
const READPROC_GID: u32 = 3009;
const MAXPACKET: u32 = 3072;

pub fn socket_addr() -> SocketAddr {
    SocketAddr::from_abstract_name(message::SOCKET_NAME).expect("Invalid abstract socket name")
}

/// Check that SELinux is enforcing and our policy is loaded: the policy
/// denies the daemon connecting to itself.
fn check_selinux() -> Result<()> {
    let mut file =
        File::open(SELINUX_ENFORCE).with_context(|| format!("Failed to open {SELINUX_ENFORCE}"))?;
    let statfs = rustix::fs::fstatfs(&file).context("Failed to statfs selinuxfs")?;
    #[allow(clippy::unnecessary_cast)]
    if statfs.f_type as u32 != SELINUX_MAGIC {
        bail!("{SELINUX_ENFORCE} is not on selinuxfs");
    }
    if file.read_u8().context("Failed to read SELinux mode")? != b'1' {
        bail!("Denying connection because SELinux is not enforcing");
    }

    match UnixStream::connect_addr(&socket_addr()) {
        Ok(_) => bail!("Denying connection because the SELinux policy is not loaded"),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Ok(()),
        Err(e) => Err(e).context("Self connection failed for unexpected reason"),
    }
}

/// Switch from root to the system user, keeping only the capabilities we
/// need. Must run before any other thread exists, since the credential
/// syscalls only affect the calling thread.
fn drop_privileges() -> Result<()> {
    let system_uid = Uid::from_raw(SYSTEM_UID);
    let system_gid = Gid::from_raw(SYSTEM_UID);
    let needed = CapabilitySet::CHOWN | CapabilitySet::SYS_NICE;

    let uid = rustix::process::getuid();
    if uid == system_uid {
        let caps = rustix::thread::capabilities(None).context("Failed to query capabilities")?;
        if !caps.permitted.contains(needed) {
            bail!("CAP_CHOWN and CAP_SYS_NICE are required when running as the system user");
        }
    } else if uid.is_root() {
        rustix::thread::set_keep_capabilities(true)
            .context("Failed to set keep capabilities flag")?;
        rustix::thread::set_thread_groups(&[Gid::from_raw(READPROC_GID)])
            .context("Failed to set supplementary groups")?;
        rustix::thread::set_thread_res_gid(system_gid, system_gid, system_gid)
            .context("Failed to switch to the system group")?;
        rustix::thread::set_thread_res_uid(system_uid, system_uid, system_uid)
            .context("Failed to switch to the system user")?;
    } else {
        bail!(
            "Must run as root or the system user, not uid {}",
            uid.as_raw()
        );
    }

    rustix::thread::set_capabilities(
        None,
        CapabilitySets {
            effective: needed,
            permitted: needed,
            inheritable: CapabilitySet::empty(),
        },
    )
    .context("Failed to drop capabilities")?;

    Ok(())
}

struct Daemon {
    gadget: Gadget,
    auth: Authenticator,
    session: Mutex<Option<Session>>,
    /// Outlives sessions, so the app's source connection survives
    /// disable/enable cycles.
    source: Arc<SourceHub>,
    udc: Option<String>,
    check_selinux: bool,
}

impl Daemon {
    fn stop_session(session: &mut Option<Session>) -> Result<()> {
        match session.take() {
            Some(s) => s.stop(),
            None => Ok(()),
        }
    }

    fn status(&self) -> Status {
        let mut session = self.session.lock().unwrap();
        // A session whose stream thread died has already failed; clean up.
        if session.as_ref().is_some_and(Session::is_finished) {
            warn!("Stream thread exited; disabling");
            if let Err(e) = Self::stop_session(&mut session) {
                warn!("{e:?}");
            }
        }

        match &*session {
            Some(s) => Status {
                enabled: true,
                stream: s.status(),
            },
            None => Status {
                enabled: false,
                stream: StreamStatus::default(),
            },
        }
    }

    fn enable(&self, request: &EnableRequest) -> Result<()> {
        let mut session = self.session.lock().unwrap();
        if let Err(e) = Self::stop_session(&mut session) {
            warn!("Previous session failed: {e:?}");
        }

        // Control values belong to a session (and its camera).
        self.source.reset_controls();
        *session = Some(Session::start(
            self.gadget.clone(),
            SessionConfig {
                specs: request.formats.clone(),
                controls: request.controls,
                endpoint: EndpointConfig {
                    transfer: Transfer::Isoc,
                    maxpacket: MAXPACKET,
                    maxburst: request.maxburst.into(),
                },
                udc: self.udc.clone(),
                irq_boost: true,
            },
            self.source.clone(),
        )?);
        info!("Enabled");
        Ok(())
    }

    fn disable(&self) -> Result<()> {
        let result = Self::stop_session(&mut self.session.lock().unwrap());
        info!("Disabled");
        result
    }

    fn handle_request(&self, request: &Request) -> Response {
        let result = match request {
            Request::GetStatus => return Response::Status(self.status()),
            Request::Enable(r) => self.enable(r),
            Request::Disable => self.disable(),
            Request::AttachSource => unreachable!("handled in handle_client"),
        };

        match result {
            Ok(()) => Response::Ok,
            Err(e) => {
                warn!("{e:?}");
                Response::Error(format!("{e:#}"))
            }
        }
    }

    /// Read frames from an attached source until it disconnects. The client
    /// was authorized when it attached; frames are not re-authorized because
    /// they can only change what the host sees, not the gadget.
    fn serve_source(&self, stream: UnixStream) -> Result<()> {
        let writer = stream
            .try_clone()
            .context("Failed to clone source socket")?;
        let id = self.source.attach(writer)?;
        let mut reader = io::BufReader::with_capacity(256 * 1024, stream);

        let result = loop {
            match Frame::read_from(&mut reader) {
                Ok(frame) => self.source.submit(id, frame),
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break Ok(()),
                Err(e) => break Err(e).context("Failed to receive frame"),
            }
        };

        self.source.detach(id);
        result
    }

    fn handle_client(&self, mut stream: UnixStream, uid: Uid) -> Result<()> {
        if self.check_selinux {
            check_selinux()?;
        }

        let version = stream
            .read_u8()
            .context("Failed to receive protocol version")?;
        if version != message::PROTOCOL_VERSION {
            stream.write_u8(0)?;
            bail!("Unsupported client protocol version: {version}");
        }
        stream.write_u8(1)?;

        loop {
            let request = match Request::read_from(&mut stream) {
                Ok(r) => r,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e).context("Failed to receive request"),
            };

            // Revalidate on every request, e.g. after package changes.
            self.auth.authorize(uid)?;
            if request == Request::AttachSource {
                Response::Ok.write_to(&mut stream)?;
                return self.serve_source(stream);
            }

            // The app polls the status every second.
            if request == Request::GetStatus {
                trace!(?request, "Request");
            } else {
                debug!(?request, "Request");
            }

            let response = self.handle_request(&request);
            if request == Request::GetStatus {
                trace!(?response, "Response");
            } else {
                debug!(?response, "Response");
            }
            response
                .write_to(&mut stream)
                .context("Failed to send response")?;
        }
    }
}

pub fn subcommand_daemon(cli: &DaemonCli) -> Result<()> {
    // Grab the supervisor pipes and drop root before anything else.
    let auth = Authenticator::new()?;
    drop_privileges()?;

    let gadget = Gadget::new(&cli.gadget, &cli.config)?;
    if gadget.function_exists() || gadget.is_linked() {
        warn!("Removing UVC function left over from a previous run");
        teardown(&gadget, None)?;
    }

    let stop = Arc::new(AtomicBool::new(false));
    for signal in [libc::SIGINT, libc::SIGTERM] {
        signal_hook::flag::register(signal, stop.clone())
            .with_context(|| format!("Failed to register handler for signal {signal}"))?;
    }

    let listener =
        UnixListener::bind_addr(&socket_addr()).context("Failed to listen on domain socket")?;
    listener
        .set_nonblocking(true)
        .context("Failed to make listener non-blocking")?;
    info!("Listening on @{}", message::SOCKET_NAME);

    let daemon = Arc::new(Daemon {
        gadget,
        auth,
        session: Mutex::new(None),
        source: Arc::new(SourceHub::default()),
        udc: cli.udc.clone(),
        check_selinux: !cli.no_selinux_check,
    });

    while !stop.load(Ordering::Relaxed) {
        let mut fds = [PollFd::new(&listener, PollFlags::IN)];
        let timeout = Timespec {
            tv_sec: 0,
            tv_nsec: 200_000_000,
        };
        match poll(&mut fds, Some(&timeout)) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(e).context("poll() failed"),
        }

        let stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e).context("Failed to accept connection"),
        };
        stream.set_nonblocking(false)?;

        let ucred = rustix::net::sockopt::socket_peercred(&stream)
            .context("Failed to get socket peer credentials")?;
        if ucred.pid == rustix::process::getpid() {
            error!("SELinux policy is broken; able to connect to self");
            continue;
        }
        // Reject unauthorized clients before reading anything from them.
        if let Err(e) = daemon.auth.authorize(ucred.uid) {
            warn!(uid = ucred.uid.as_raw(), "Rejected connection: {e:#}");
            continue;
        }

        let daemon = daemon.clone();
        thread::spawn(move || {
            let _span = info_span!(
                "peer",
                pid = ucred.pid.as_raw_nonzero(),
                uid = ucred.uid.as_raw(),
            )
            .entered();
            info!("Client connected");
            if let Err(e) = daemon.handle_client(stream, ucred.uid) {
                warn!("Client failed: {e:?}");
            }
        });
    }

    info!("Shutting down");
    daemon.disable()
}

/// Run the daemon (normally started by the module's supervisor).
#[derive(Debug, Parser)]
pub struct DaemonCli {
    /// Root of the configfs USB gadget.
    #[arg(long, value_name = "PATH", default_value = "/config/usb_gadget/g1")]
    gadget: PathBuf,

    /// Gadget configuration to add the UVC function to.
    #[arg(long, value_name = "NAME", default_value = "b.1")]
    config: String,

    /// USB device controller to bind (default: the gadget's current one).
    #[arg(long, value_name = "NAME")]
    udc: Option<String>,

    /// Serve clients even if SELinux is permissive or our policy is not
    /// loaded. Only for collecting denials while developing the policy.
    #[arg(long)]
    no_selinux_check: bool,
}

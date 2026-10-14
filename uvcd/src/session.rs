// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! Lifecycle of our UVC function on the gadget: create and bind it, serve
//! the host from a background thread, and restore the original USB
//! configuration afterwards. Used by both `gadget up` and the daemon.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use tracing::{error, info, warn};

use crate::{
    gadget::{DEVICE_CLASS_IAD, EndpointConfig, Gadget, resolve_udc},
    irq::IrqBoost,
    source::SourceHub,
    stream::{Exit, StreamStatus, Streamer},
    uvc::{
        EntityIds, Protocol,
        controls::ControlCaps,
        format::{FormatSpec, FormatTable},
    },
    v4l2::{Speed, VideoDevice},
};

const NODE_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub specs: Vec<FormatSpec>,
    pub endpoint: EndpointConfig,
    /// Optional controls to advertise.
    pub controls: ControlCaps,
    /// Controller to bind; the gadget's current one by default.
    pub udc: Option<String>,
    /// Raise the controller's IRQ thread to real-time priority while
    /// streaming.
    pub irq_boost: bool,
}

/// The gadget settings a session changes and restores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Original {
    pub udc: String,
    pub device_class: [u8; 3],
}

/// Undo a session: detach our function from the config, restore the device
/// class, rebind the controller, then delete the function. Without
/// `original` (e.g. after a crash), the controller is rebound to whatever
/// is configured and an IAD device class we set is reset to 0.
pub fn teardown(gadget: &Gadget, original: Option<&Original>) -> Result<()> {
    if gadget.is_linked() {
        let udc = original.map(|o| o.udc.clone()).or(gadget.udc()?);
        let class = match original {
            Some(o) => Some(o.device_class),
            None if gadget.device_class()? == DEVICE_CLASS_IAD => Some([0; 3]),
            None => None,
        };

        gadget.set_udc(None)?;
        gadget.unlink_function()?;
        if let Some(class) = class {
            gadget.set_device_class(class)?;
        }
        if let Some(udc) = udc {
            gadget.set_udc(Some(&udc))?;
        }
        info!("Detached UVC function from the gadget");
    }

    if gadget.function_exists() {
        gadget.remove_function()?;
        info!("Removed UVC function");
    }

    Ok(())
}

pub struct Session {
    gadget: Gadget,
    original: Original,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
    status: Arc<Mutex<StreamStatus>>,
}

impl Session {
    /// Add our function to the gadget and start serving the host. The
    /// gadget must not have a leftover function from a previous session.
    pub fn start(gadget: Gadget, config: SessionConfig, source: Arc<SourceHub>) -> Result<Self> {
        let udc = resolve_udc(&gadget, config.udc.as_deref())?;
        let original = Original {
            udc: udc.clone(),
            device_class: gadget.device_class()?,
        };

        let attach = || -> Result<(FormatTable, EntityIds)> {
            let (table, entities) =
                gadget.create_function(&config.specs, &config.endpoint, &config.controls)?;
            info!("Created UVC function: {table:?}");

            // The UDC must be unbound to change the config. This briefly
            // disconnects every USB function, including ADB over USB.
            gadget.set_udc(None)?;
            gadget.link_function()?;
            gadget.set_device_class(DEVICE_CLASS_IAD)?;
            gadget.set_udc(Some(&udc))?;
            info!("Bound gadget to {udc}");

            Ok((table, entities))
        };

        let (table, entities) = match attach() {
            Ok(r) => r,
            Err(e) => {
                if let Err(te) = teardown(&gadget, Some(&original)) {
                    warn!("Failed to clean up after error: {te:?}");
                }
                return Err(e);
            }
        };

        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(StreamStatus::default()));
        let thread = {
            let server = Server {
                gadget: gadget.clone(),
                controls: config.controls,
                table,
                entities,
                endpoint: config.endpoint,
                irq_udc: config.irq_boost.then_some(udc),
                stop: stop.clone(),
                status: status.clone(),
                source,
            };
            thread::Builder::new()
                .name("uvc-stream".into())
                .spawn(move || server.run())
                .context("Failed to spawn stream thread")?
        };

        Ok(Self {
            gadget,
            original,
            stop,
            thread: Some(thread),
            status,
        })
    }

    pub fn original(&self) -> &Original {
        &self.original
    }

    pub fn status(&self) -> StreamStatus {
        self.status.lock().unwrap().clone()
    }

    /// Whether the stream thread exited on its own, i.e. failed.
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// Stop serving and restore the original USB configuration. Returns the
    /// stream thread's error, if it failed.
    pub fn stop(mut self) -> Result<()> {
        self.shutdown()
    }

    fn shutdown(&mut self) -> Result<()> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };

        self.stop.store(true, Ordering::Relaxed);
        let result = thread
            .join()
            .map_err(|_| anyhow!("Stream thread panicked"))
            .and_then(|r| r);
        if let Err(e) = &result {
            error!("{e:?}");
        }

        // Always restore the original USB configuration.
        teardown(&self.gadget, Some(&self.original))?;
        result
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Err(e) = self.shutdown() {
            warn!("Session shutdown failed: {e:?}");
        }
    }
}

/// What the stream thread needs.
struct Server {
    gadget: Gadget,
    controls: ControlCaps,
    table: FormatTable,
    entities: EntityIds,
    endpoint: EndpointConfig,
    /// Controller whose IRQ thread to boost, if enabled.
    irq_udc: Option<String>,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<StreamStatus>>,
    source: Arc<SourceHub>,
}

impl Server {
    /// Open the video node and serve the host, reopening it whenever the
    /// gadget is rebound, until `stop` is set.
    fn run(self) -> Result<()> {
        while !self.stop.load(Ordering::Relaxed) {
            let node = match self.gadget.wait_for_video_node(NODE_WAIT) {
                Ok(n) => n,
                Err(e) => {
                    // Something else (e.g. a USB mode switch) unbound the
                    // gadget. Our function stays linked; wait for a rebind.
                    warn!("{e}; waiting");
                    continue;
                }
            };

            let dev = VideoDevice::open(&node)?;
            let (driver, card) = dev.query_capability()?;
            info!("Opened {node:?} ({driver}, {card})");
            // This activates the function and connects to the host.
            dev.subscribe_uvc_events()?;

            let mut protocol = Protocol::new(
                self.table.clone(),
                self.entities,
                self.endpoint.max_payload(Speed::High),
                self.controls,
            );
            // Keep host-set controls across rebinds.
            for control in self.source.controls() {
                protocol.set_control(control);
            }
            let irq_boost = self.irq_udc.as_deref().map(IrqBoost::new);
            let exit = Streamer::new(
                dev,
                protocol,
                self.endpoint,
                irq_boost,
                self.status.clone(),
                self.source.clone(),
            )
            .run(&self.stop)?;
            *self.status.lock().unwrap() = StreamStatus::default();
            match exit {
                Exit::Stopped => break,
                Exit::DeviceGone => warn!("Video node went away; reopening"),
            }
        }

        info!("Stopping");
        Ok(())
    }
}

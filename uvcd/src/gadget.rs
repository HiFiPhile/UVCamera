// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! configfs USB gadget manipulation: binding, config links, and building the
//! UVC function's descriptor tree.

use std::{
    fs, io,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use tracing::{debug, warn};

use crate::uvc::{
    EntityIds,
    controls::ControlCaps,
    format::{Format, FormatSpec, FormatTable, Frame},
};

/// Our function instance: `functions/<function>.<INSTANCE>`.
pub const INSTANCE: &str = "uvcamera";
/// The kernel's built-in UVC function.
pub const FUNCTION_UVC: &str = "uvc";
/// Our `usb_f_uvcx` module: upstream f_uvc with frame-based formats and bulk
/// streaming.
pub const FUNCTION_UVCX: &str = "uvcx";
/// Name of our link in the config. Vendor init scripts only remove `f1`..`f9`.
pub const CONFIG_LINK: &str = "uvcamera";
/// Interface string shown by hosts.
const INTERFACE_NAME: &str = "UVCamera";

/// Misc / Common / Interface Association Descriptor, required for Windows to
/// group the UVC interfaces of a composite device.
pub const DEVICE_CLASS_IAD: [u8; 3] = [0xef, 0x02, 0x01];
const DEVICE_CLASS_ATTRS: [&str; 3] = ["bDeviceClass", "bDeviceSubClass", "bDeviceProtocol"];

fn read_attr(path: &Path) -> Result<String> {
    fs::read_to_string(path)
        .map(|s| s.trim_end().to_owned())
        .with_context(|| format!("Failed to read {path:?}"))
}

fn write_attr(path: &Path, value: &str) -> Result<()> {
    fs::write(path, value).with_context(|| format!("Failed to write {value:?} to {path:?}"))
}

/// Whether a configfs attribute can be written: attributes without a store
/// method have no write bits (and refuse writes even with CAP_DAC_OVERRIDE).
fn is_writable(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path).with_context(|| format!("Failed to stat {path:?}"))?;
    Ok(metadata.permissions().mode() & 0o222 != 0)
}

fn parse_u8(s: &str) -> Result<u8> {
    let parsed = match s.strip_prefix("0x") {
        Some(hex) => u8::from_str_radix(hex, 16),
        None => s.parse(),
    };
    parsed.with_context(|| format!("Invalid u8 value: {s:?}"))
}

/// Remove a file/symlink/dir, treating "not found" as success.
fn remove(path: &Path, dir: bool) -> Result<()> {
    let ret = if dir {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    };
    match ret {
        Ok(()) => {
            debug!("Removed {path:?}");
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to remove {path:?}")),
    }
}

/// Create a configfs directory. configfs always creates nodes as root:root,
/// so when running unprivileged (the daemon runs as `system` with
/// CAP_CHOWN), hand the new directory and everything the kernel populated in
/// it over to our uid/gid.
fn mkdir(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("Failed to create {path:?}")),
    }

    let uid = rustix::process::getuid();
    if !uid.is_root() {
        chown_recursive(path, uid, rustix::process::getgid())?;
    }
    Ok(())
}

fn chown_recursive(path: &Path, uid: rustix::fs::Uid, gid: rustix::fs::Gid) -> Result<()> {
    rustix::fs::chownat(
        rustix::fs::CWD,
        path,
        Some(uid),
        Some(gid),
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .with_context(|| format!("Failed to chown {path:?}"))?;

    if path.symlink_metadata().is_ok_and(|m| m.is_dir()) {
        for entry in fs::read_dir(path).with_context(|| format!("Failed to read {path:?}"))? {
            let entry = entry.with_context(|| format!("Failed to read {path:?}"))?;
            chown_recursive(&entry.path(), uid, gid)?;
        }
    }
    Ok(())
}

fn link(target: &Path, link: &Path) -> Result<()> {
    // configfs resolves relative targets against the cwd, so always use
    // absolute paths.
    symlink(target, link).with_context(|| format!("Failed to link {link:?} -> {target:?}"))
}

/// A configfs gadget such as `/config/usb_gadget/g1`.
#[derive(Debug, Clone)]
pub struct Gadget {
    root: PathBuf,
    config: String,
    /// USB function driver: [`FUNCTION_UVC`] or [`FUNCTION_UVCX`].
    function: String,
}

impl Gadget {
    pub fn new(root: impl Into<PathBuf>, config: impl Into<String>) -> Result<Self> {
        let root = root.into();
        if !root.join("functions").is_dir() {
            bail!("Not a configfs gadget: {root:?}");
        }
        Ok(Self {
            root,
            config: config.into(),
            function: FUNCTION_UVC.into(),
        })
    }

    /// Use another UVC function driver than the built-in one.
    pub fn with_function(mut self, function: impl Into<String>) -> Self {
        self.function = function.into();
        self
    }

    pub fn function_dir(&self) -> PathBuf {
        self.root
            .join("functions")
            .join(format!("{}.{INSTANCE}", self.function))
    }

    fn config_link(&self) -> PathBuf {
        self.root
            .join("configs")
            .join(&self.config)
            .join(CONFIG_LINK)
    }

    pub fn udc(&self) -> Result<Option<String>> {
        let udc = read_attr(&self.root.join("UDC"))?;
        Ok(if udc.is_empty() { None } else { Some(udc) })
    }

    /// Bind to a USB device controller, or unbind with `None`. Idempotent.
    pub fn set_udc(&self, udc: Option<&str>) -> Result<()> {
        let path = self.root.join("UDC");
        match fs::write(&path, format!("{}\n", udc.unwrap_or_default())) {
            Ok(()) => Ok(()),
            // Unbinding an unbound gadget fails with ENODEV.
            Err(e) if udc.is_none() && e.raw_os_error() == Some(libc::ENODEV) => Ok(()),
            Err(e) => Err(e).with_context(|| format!("Failed to write {udc:?} to {path:?}")),
        }
    }

    pub fn device_class(&self) -> Result<[u8; 3]> {
        let mut class = [0; 3];
        for (value, attr) in class.iter_mut().zip(DEVICE_CLASS_ATTRS) {
            *value = parse_u8(&read_attr(&self.root.join(attr))?)?;
        }
        Ok(class)
    }

    pub fn set_device_class(&self, class: [u8; 3]) -> Result<()> {
        for (value, attr) in class.into_iter().zip(DEVICE_CLASS_ATTRS) {
            write_attr(&self.root.join(attr), &format!("{value:#04x}"))?;
        }
        Ok(())
    }

    pub fn is_linked(&self) -> bool {
        self.config_link().symlink_metadata().is_ok()
    }

    pub fn link_function(&self) -> Result<()> {
        if !self.is_linked() {
            link(&self.function_dir(), &self.config_link())?;
        }
        Ok(())
    }

    pub fn unlink_function(&self) -> Result<()> {
        remove(&self.config_link(), false)
    }

    pub fn function_exists(&self) -> bool {
        self.function_dir().is_dir()
    }

    /// Put our function back into the config after something else rebuilt
    /// it without us (a USB mode switch that clears every function link).
    /// Waits for that to finish: only acts once the gadget is bound again.
    /// Returns whether it re-linked.
    pub fn relink_if_dropped(&self) -> Result<bool> {
        if self.is_linked() || !self.function_exists() {
            return Ok(false);
        }
        let Some(udc) = self.udc()? else {
            return Ok(false);
        };
        self.set_udc(None)?;
        self.link_function()?;
        self.set_device_class(DEVICE_CLASS_IAD)?;
        self.set_udc(Some(&udc))?;
        Ok(true)
    }

    /// Create the UVC function with the given formats. Returns the format
    /// table with kernel-assigned indexes, the VideoControl entity IDs and
    /// the controls actually advertised (see below).
    pub fn create_function(
        &self,
        specs: &[FormatSpec],
        endpoint: &EndpointConfig,
        controls: &ControlCaps,
    ) -> Result<(FormatTable, EntityIds, ControlCaps)> {
        let f = self.function_dir();
        mkdir(&f)?;

        // Advertise the controls the camera supports. The bitmaps are
        // writable since Linux 6.x (the OnePlus 15's 6.12 GKI allows it);
        // older kernels keep their defaults, brightness and AE mode, which
        // is what no optional controls means.
        let bitmap = |bytes: Vec<u8>| bytes.iter().map(|b| format!("{b}\n")).collect::<String>();
        let terminal_controls = f.join("control/terminal/camera/default/bmControls");
        let controls = if is_writable(&terminal_controls)? {
            write_attr(
                &terminal_controls,
                &bitmap(controls.camera_terminal_bm_controls()),
            )?;
            write_attr(
                &f.join("control/processing/default/bmControls"),
                &bitmap(controls.processing_unit_bm_controls()),
            )?;
            *controls
        } else {
            if *controls != ControlCaps::default() {
                warn!("This kernel's UVC controls are fixed: advertising brightness only");
            }
            ControlCaps::default()
        };

        let transfer = f.join("streaming_transfer");
        match endpoint.transfer {
            Transfer::Isoc => {
                if transfer.exists() {
                    write_attr(&transfer, "isoc")?;
                }
            }
            Transfer::Bulk => {
                if !transfer.exists() {
                    bail!("The {} function has no bulk streaming", self.function);
                }
                write_attr(&transfer, "bulk")?;
            }
        }
        write_attr(
            &f.join("streaming_maxpacket"),
            &endpoint.maxpacket.to_string(),
        )?;
        write_attr(
            &f.join("streaming_maxburst"),
            &endpoint.maxburst.to_string(),
        )?;
        write_attr(&f.join("streaming_interval"), "1")?;
        // Only on kernels >= 6.2; older ones use a fixed string.
        if f.join("function_name").exists() {
            write_attr(&f.join("function_name"), INTERFACE_NAME)?;
        }

        // VideoControl: header linked into the full/super speed class
        // descriptors. Terminals and the processing unit keep kernel defaults.
        let control = f.join("control");
        let control_header = control.join("header/h");
        mkdir(&control_header)?;
        for speed in ["fs", "ss"] {
            link(
                &control_header,
                &control.join("class").join(speed).join("h"),
            )?;
        }

        // VideoStreaming: formats and frames, then the header linking them,
        // then the header linked into each speed. The kernel numbers formats
        // and frames when the header is linked into a class.
        let streaming = f.join("streaming");
        let streaming_header = streaming.join("header/h");
        let mut format_dirs = vec![];

        for spec in specs {
            let format_dir = streaming
                .join(spec.kind.configfs_dir())
                .join(spec.kind.configfs_name());
            mkdir(&format_dir)?;

            for frame in &spec.frames {
                let d = format_dir.join(frame.configfs_name());
                mkdir(&d)?;

                let max_size = spec.kind.max_frame_size(frame.width, frame.height);
                let bits =
                    |interval: u32| u64::from(max_size) * 8 * 10_000_000 / u64::from(interval);
                let intervals = frame
                    .intervals
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>();

                write_attr(&d.join("wWidth"), &frame.width.to_string())?;
                write_attr(&d.join("wHeight"), &frame.height.to_string())?;
                write_attr(&d.join("dwMaxVideoFrameBufferSize"), &max_size.to_string())?;
                write_attr(
                    &d.join("dwMaxBitRate"),
                    &bits(frame.intervals[0]).to_string(),
                )?;
                write_attr(
                    &d.join("dwMinBitRate"),
                    &bits(*frame.intervals.last().unwrap()).to_string(),
                )?;
                write_attr(
                    &d.join("dwDefaultFrameInterval"),
                    &frame.default_interval().to_string(),
                )?;
                write_attr(&d.join("dwFrameInterval"), &(intervals.join("\n") + "\n"))?;
            }

            format_dirs.push(format_dir);
        }

        mkdir(&streaming_header)?;
        for (spec, format_dir) in specs.iter().zip(&format_dirs) {
            link(
                format_dir,
                &streaming_header.join(spec.kind.configfs_name()),
            )?;
        }
        for speed in ["fs", "hs", "ss"] {
            link(
                &streaming_header,
                &streaming.join("class").join(speed).join("h"),
            )?;
        }

        // Read back the numbering the host will see.
        let mut formats = vec![];
        for (spec, format_dir) in specs.iter().zip(&format_dirs) {
            let index = parse_u8(&read_attr(&format_dir.join("bFormatIndex"))?)?;
            let mut frames = vec![];
            for frame in &spec.frames {
                let d = format_dir.join(frame.configfs_name());
                frames.push(Frame {
                    index: parse_u8(&read_attr(&d.join("bFrameIndex"))?)?,
                    spec: frame.clone(),
                });
            }
            formats.push(Format {
                index,
                kind: spec.kind,
                frames,
            });
        }

        let entities = EntityIds {
            camera_terminal: parse_u8(&read_attr(
                &control.join("terminal/camera/default/bTerminalID"),
            )?)?,
            processing_unit: parse_u8(&read_attr(&control.join("processing/default/bUnitID"))?)?,
        };

        Ok((FormatTable::new(formats)?, entities, controls))
    }

    /// Remove the UVC function, including partially created ones. The
    /// function must not be linked into a config.
    pub fn remove_function(&self) -> Result<()> {
        let f = self.function_dir();
        if !f.is_dir() {
            return Ok(());
        }

        let streaming = f.join("streaming");
        for speed in ["fs", "hs", "ss"] {
            remove(&streaming.join("class").join(speed).join("h"), false)?;
        }

        let streaming_header = streaming.join("header/h");
        if let Ok(entries) = fs::read_dir(&streaming_header) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|t| t.is_symlink()) {
                    remove(&entry.path(), false)?;
                }
            }
        }
        remove(&streaming_header, true)?;

        for (dir, name) in [("mjpeg", "m"), ("uncompressed", "u")] {
            let format_dir = streaming.join(dir).join(name);
            if let Ok(entries) = fs::read_dir(&format_dir) {
                for entry in entries.flatten() {
                    if entry.file_type().is_ok_and(|t| t.is_dir()) {
                        remove(&entry.path(), true)?;
                    }
                }
            }
            remove(&format_dir, true)?;
        }

        let control = f.join("control");
        for speed in ["fs", "ss"] {
            remove(&control.join("class").join(speed).join("h"), false)?;
        }
        remove(&control.join("header/h"), true)?;

        remove(&f, true)
    }

    /// Find the video node created for our function after binding.
    pub fn wait_for_video_node(&self, timeout: Duration) -> Result<PathBuf> {
        let deadline = Instant::now() + timeout;

        loop {
            if let Some(path) = find_video_node(&self.function)? {
                return Ok(path);
            }
            if Instant::now() >= deadline {
                bail!(
                    "Video node for {}.{INSTANCE} did not appear within {timeout:?}",
                    self.function
                );
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Look for `/sys/class/video4linux/video*` whose `function_name` is our
/// function instance, and return its `/dev` node once ueventd created it.
fn find_video_node(function: &str) -> Result<Option<PathBuf>> {
    const CLASS: &str = "/sys/class/video4linux";

    let entries = match fs::read_dir(CLASS) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("Failed to read {CLASS}")),
    };

    for entry in entries.flatten() {
        let Ok(name) = fs::read_to_string(entry.path().join("function_name")) else {
            continue;
        };
        // The attribute is the configfs item name, i.e. "uvc.<instance>".
        let name = name.trim_end();
        if name == format!("{function}.{INSTANCE}") || name == INSTANCE {
            let dev = Path::new("/dev").join(entry.file_name());
            return Ok(dev.exists().then_some(dev));
        }
    }

    Ok(None)
}

/// How the streaming endpoint moves video data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Transfer {
    /// Isochronous: reserved bandwidth, lost packets are not retried.
    #[default]
    Isoc,
    /// Bulk: whatever bandwidth is free, with retries. Needs `uvcx`.
    Bulk,
}

/// Largest bulk payload `uvcx` accepts.
pub const BULK_MAX_PAYLOAD: u32 = 16 * 1024 * 1024;

/// Streaming endpoint configuration.
#[derive(Debug, Clone, Copy)]
pub struct EndpointConfig {
    pub transfer: Transfer,
    /// `streaming_maxpacket`. Isochronous: 1..=3072; above 1024,
    /// high-speed uses high-bandwidth (multiple transactions per
    /// microframe). Bulk: the payload size, rounded up to 1024.
    pub maxpacket: u32,
    /// `streaming_maxburst`: 0..=15, SuperSpeed only.
    pub maxburst: u32,
}

impl EndpointConfig {
    /// Isochronous: bytes per service interval at `speed`, mirroring f_uvc's
    /// descriptor setup. Bulk: the payload size. This is what
    /// dwMaxPayloadTransferSize must not exceed.
    pub fn max_payload(&self, speed: crate::v4l2::Speed) -> u32 {
        use crate::v4l2::Speed;

        if self.transfer == Transfer::Bulk {
            return self
                .maxpacket
                .div_ceil(1024)
                .clamp(1, BULK_MAX_PAYLOAD / 1024)
                * 1024;
        }

        let maxburst = self.maxburst.min(15);
        let mut maxpacket = self.maxpacket.clamp(1, 3072);
        if maxburst > 0 && !maxpacket.is_multiple_of(1024) {
            maxpacket = maxpacket.div_ceil(1024) * 1024;
        }

        match speed {
            Speed::Low | Speed::Full | Speed::Unknown => maxpacket.min(1023),
            Speed::High | Speed::Wireless => maxpacket,
            Speed::Super | Speed::SuperPlus => maxpacket * (maxburst + 1),
        }
    }
}

/// Pick the controller to bind to: the explicit one, the gadget's current
/// one, or the only one in `/sys/class/udc`.
pub fn resolve_udc(gadget: &Gadget, explicit: Option<&str>) -> Result<String> {
    if let Some(udc) = explicit {
        return Ok(udc.to_owned());
    }
    if let Some(udc) = gadget.udc()? {
        return Ok(udc);
    }

    let udcs = fs::read_dir("/sys/class/udc")
        .context("Failed to list /sys/class/udc")?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    match udcs.as_slice() {
        [udc] => Ok(udc.clone()),
        [] => Err(anyhow!("No USB device controller found")),
        _ => {
            warn!("Multiple UDCs: {udcs:?}");
            Err(anyhow!("Multiple USB device controllers; pass --udc"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v4l2::Speed;

    #[test]
    fn max_payload_matches_f_uvc() {
        let hs = EndpointConfig {
            transfer: Transfer::Isoc,
            maxpacket: 3072,
            maxburst: 0,
        };
        assert_eq!(hs.max_payload(Speed::High), 3072);
        assert_eq!(hs.max_payload(Speed::Super), 3072);
        assert_eq!(hs.max_payload(Speed::Full), 1023);

        let ss = EndpointConfig {
            transfer: Transfer::Isoc,
            maxpacket: 3000,
            maxburst: 6,
        };
        assert_eq!(ss.max_payload(Speed::Super), 3072 * 7);
        assert_eq!(ss.max_payload(Speed::High), 3072);

        let bulk = EndpointConfig {
            transfer: Transfer::Bulk,
            maxpacket: 100_000,
            maxburst: 15,
        };
        assert_eq!(bulk.max_payload(Speed::Super), 100_352);
        assert_eq!(bulk.max_payload(Speed::High), 100_352);
    }

    #[test]
    fn relinks_only_after_the_gadget_is_bound_again() {
        let root = std::env::temp_dir().join(format!("uvcd-gadget-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let gadget_dir = root.join("g1");
        fs::create_dir_all(gadget_dir.join("configs/b.1")).unwrap();
        fs::create_dir_all(gadget_dir.join(format!("functions/uvc.{INSTANCE}"))).unwrap();
        for attr in DEVICE_CLASS_ATTRS {
            fs::write(gadget_dir.join(attr), "0x00").unwrap();
        }
        fs::write(gadget_dir.join("UDC"), "\n").unwrap();
        let gadget = Gadget::new(&gadget_dir, "b.1").unwrap();

        // Mid-switch: unlinked and unbound. Leave it alone.
        assert!(!gadget.relink_if_dropped().unwrap());
        assert!(!gadget.is_linked());

        // The switch finished without us.
        fs::write(gadget_dir.join("UDC"), "a600000.dwc3\n").unwrap();
        assert!(gadget.relink_if_dropped().unwrap());
        assert!(gadget.is_linked());
        assert_eq!(gadget.device_class().unwrap(), DEVICE_CLASS_IAD);
        assert_eq!(gadget.udc().unwrap().as_deref(), Some("a600000.dwc3"));

        // Linked: nothing to do.
        assert!(!gadget.relink_if_dropped().unwrap());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn parses_configfs_u8() {
        assert_eq!(parse_u8("0xef").unwrap(), 0xef);
        assert_eq!(parse_u8("0x00").unwrap(), 0);
        assert_eq!(parse_u8("2").unwrap(), 2);
    }
}

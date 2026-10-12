// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! Read-only report of whether this device can act as a UVC gadget.
//!
//! This is the first thing to run on a new device. Nothing here modifies the
//! USB configuration. The optional `--probe-function` check creates and
//! immediately removes an unlinked UVC function instance in configfs, which is
//! the only definitive test for kernel support when `/proc/config.gz` is
//! missing.

use std::{
    collections::BTreeMap,
    fmt, fs,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use clap::Parser;
use flate2::read::GzDecoder;

const KERNEL_CONFIG: &str = "/proc/config.gz";
const SELINUX_ENFORCE: &str = "/sys/fs/selinux/enforce";
const UDC_CLASS: &str = "/sys/class/udc";
const V4L2_CLASS: &str = "/sys/class/video4linux";
const GADGET_HAL_PREFIX: &str = "android.hardware.usb.gadget";
const PROBE_FUNCTION: &str = "uvc.uvcamera_probe";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Info,
    Warn,
    Fail,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pass => "PASS",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        })
    }
}

#[derive(Default)]
struct Report {
    failed: bool,
}

impl Report {
    fn add(&mut self, status: Status, name: &str, detail: impl fmt::Display) {
        if status == Status::Fail {
            self.failed = true;
        }
        println!("[{status}] {name}: {detail}");
    }
}

fn read_trimmed(path: impl AsRef<Path>) -> io::Result<String> {
    fs::read_to_string(path).map(|s| s.trim_end().to_owned())
}

fn dir_names(path: impl AsRef<Path>) -> io::Result<Vec<String>> {
    let mut names = fs::read_dir(path)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    Ok(names)
}

/// Value of a tristate kernel config option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tristate {
    Yes,
    Module,
    No,
}

/// Parse the requested options out of a kernel `.config`. Options that are
/// absent or `# ... is not set` map to [`Tristate::No`].
fn parse_kernel_config(
    reader: impl BufRead,
    wanted: &[&'static str],
) -> io::Result<BTreeMap<&'static str, Tristate>> {
    let mut result = wanted
        .iter()
        .map(|&k| (k, Tristate::No))
        .collect::<BTreeMap<_, _>>();

    for line in reader.lines() {
        let line = line?;
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if let Some(state) = result.get_mut(key) {
            *state = match value {
                "y" => Tristate::Yes,
                "m" => Tristate::Module,
                _ => Tristate::No,
            };
        }
    }

    Ok(result)
}

fn check_environment(report: &mut Report) {
    let uid = rustix::process::getuid();
    if uid.is_root() {
        report.add(Status::Pass, "uid", "running as root");
    } else {
        report.add(
            Status::Warn,
            "uid",
            format!("running as uid {}; most checks need root", uid.as_raw()),
        );
    }

    let uname = rustix::system::uname();
    report.add(
        Status::Info,
        "kernel",
        format!(
            "{} {}",
            uname.release().to_string_lossy(),
            uname.machine().to_string_lossy(),
        ),
    );

    match read_trimmed(SELINUX_ENFORCE).as_deref() {
        Ok("1") => report.add(Status::Info, "selinux", "enforcing"),
        Ok("0") => report.add(Status::Info, "selinux", "permissive"),
        Ok(v) => report.add(Status::Warn, "selinux", format!("unexpected value: {v:?}")),
        Err(e) => report.add(Status::Info, "selinux", format!("unavailable: {e}")),
    }
}

fn check_kernel_config(report: &mut Report) {
    const UVC: &str = "CONFIG_USB_CONFIGFS_F_UVC";
    const OPTIONS: &[&str] = &["CONFIG_USB_CONFIGFS", UVC, "CONFIG_VIDEO_DEV"];

    let options = match fs::File::open(KERNEL_CONFIG)
        .and_then(|f| parse_kernel_config(BufReader::new(GzDecoder::new(f)), OPTIONS))
    {
        Ok(o) => o,
        Err(e) => {
            report.add(
                Status::Warn,
                "kernel config",
                format!("cannot read {KERNEL_CONFIG}: {e}; use --probe-function instead"),
            );
            return;
        }
    };

    for (name, state) in options {
        let (status, detail) = match state {
            Tristate::Yes => (Status::Pass, "built in"),
            Tristate::Module => (Status::Warn, "module; must be loadable at runtime"),
            Tristate::No if name == UVC => (Status::Fail, "not enabled; UVC gadget unsupported"),
            Tristate::No => (Status::Warn, "not enabled"),
        };
        report.add(status, name, detail);
    }
}

fn check_gadget(report: &mut Report, gadget: &Path, probe: bool) {
    if !gadget.is_dir() {
        report.add(
            Status::Fail,
            "configfs gadget",
            format!("{gadget:?} not found; legacy android_usb devices are unsupported"),
        );
        return;
    }
    report.add(Status::Pass, "configfs gadget", gadget.display());

    match read_trimmed(gadget.join("UDC")) {
        Ok(udc) if udc.is_empty() => report.add(Status::Info, "gadget UDC", "<unbound>"),
        Ok(udc) => report.add(Status::Info, "gadget UDC", udc),
        Err(e) => report.add(Status::Warn, "gadget UDC", format!("unreadable: {e}")),
    }

    let functions = match dir_names(gadget.join("functions")) {
        Ok(f) => f,
        Err(e) => {
            report.add(Status::Fail, "gadget functions", format!("unreadable: {e}"));
            return;
        }
    };
    report.add(Status::Info, "gadget functions", functions.join(" "));

    let uvc = functions
        .iter()
        .filter(|f| f.starts_with("uvc."))
        .collect::<Vec<_>>();
    if !uvc.is_empty() {
        report.add(
            Status::Info,
            "existing uvc functions",
            format!("{uvc:?} (possibly the OEM DeviceAsWebcam feature)"),
        );
    }

    let configs = gadget.join("configs");
    for config in dir_names(&configs).unwrap_or_default() {
        let links = fs::read_dir(configs.join(&config))
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_symlink()))
                    .map(|e| {
                        let target = fs::read_link(e.path()).unwrap_or_default();
                        format!(
                            "{}->{}",
                            e.file_name().to_string_lossy(),
                            target.file_name().unwrap_or_default().to_string_lossy(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        report.add(Status::Info, &format!("config {config}"), links.join(" "));
    }

    if probe {
        probe_uvc_function(report, gadget);
    }
}

/// Create and remove an unlinked UVC function instance. configfs only allows
/// this when the kernel has the UVC gadget function available.
fn probe_uvc_function(report: &mut Report, gadget: &Path) {
    let path = gadget.join("functions").join(PROBE_FUNCTION);

    if let Err(e) = fs::create_dir(&path) {
        let status = if e.kind() == io::ErrorKind::PermissionDenied {
            Status::Warn
        } else {
            Status::Fail
        };
        report.add(status, "uvc function probe", format!("mkdir {path:?}: {e}"));
        return;
    }

    let has_tree = path.join("control").is_dir() && path.join("streaming").is_dir();
    let attrs = dir_names(&path).unwrap_or_default().join(" ");

    if let Err(e) = fs::remove_dir(&path) {
        report.add(
            Status::Fail,
            "uvc function probe",
            format!("created but failed to remove {path:?}: {e}; remove it manually"),
        );
        return;
    }

    if has_tree {
        report.add(
            Status::Pass,
            "uvc function probe",
            format!("supported ({attrs})"),
        );
    } else {
        report.add(
            Status::Fail,
            "uvc function probe",
            format!("created but missing control/streaming: {attrs}"),
        );
    }
}

fn check_udcs(report: &mut Report) {
    match dir_names(UDC_CLASS) {
        Ok(udcs) if udcs.is_empty() => report.add(Status::Fail, "udc", "no USB device controllers"),
        Ok(udcs) => {
            for udc in udcs {
                let base = Path::new(UDC_CLASS).join(&udc);
                let speed = read_trimmed(base.join("maximum_speed")).unwrap_or_default();
                let state = read_trimmed(base.join("state")).unwrap_or_default();
                report.add(
                    Status::Info,
                    &format!("udc {udc}"),
                    format!("maximum_speed={speed} state={state}"),
                );
            }
        }
        Err(e) => report.add(Status::Fail, "udc", format!("cannot list {UDC_CLASS}: {e}")),
    }
}

fn check_gadget_hal(report: &mut Report) {
    let mut found = vec![];

    for pid in dir_names("/proc").unwrap_or_default() {
        if !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let argv0 = cmdline.split(|&b| b == 0).next().unwrap_or_default();
        let name = String::from_utf8_lossy(argv0);
        let base = name.rsplit('/').next().unwrap_or_default();
        if base.starts_with(GADGET_HAL_PREFIX) {
            found.push(format!("{base} (pid {pid})"));
        }
    }

    if found.is_empty() {
        report.add(
            Status::Info,
            "gadget HAL",
            "not found; init scripts likely manage the gadget",
        );
    } else {
        report.add(Status::Info, "gadget HAL", found.join(", "));
    }
}

fn check_video_nodes(report: &mut Report) {
    let nodes = match dir_names(V4L2_CLASS) {
        Ok(n) => n,
        Err(e) => {
            report.add(
                Status::Info,
                "v4l2",
                format!("cannot list {V4L2_CLASS}: {e}"),
            );
            return;
        }
    };

    for node in nodes {
        let base = Path::new(V4L2_CLASS).join(&node);
        let name = read_trimmed(base.join("name")).unwrap_or_default();
        // Only UVC gadget video nodes have this attribute.
        let detail = match read_trimmed(base.join("function_name")) {
            Ok(f) => format!("{name} (uvc gadget function {f:?})"),
            Err(_) => name,
        };
        report.add(Status::Info, &format!("v4l2 {node}"), detail);
    }
}

pub fn subcommand_doctor(cli: &DoctorCli) -> Result<()> {
    let mut report = Report::default();

    check_environment(&mut report);
    check_kernel_config(&mut report);
    check_gadget(&mut report, &cli.gadget, cli.probe_function);
    check_udcs(&mut report);
    check_gadget_hal(&mut report);
    check_video_nodes(&mut report);

    if report.failed {
        bail!("Device does not meet the requirements for UVC gadget mode");
    }

    Ok(())
}

/// Report whether this device supports the UVC gadget function.
#[derive(Debug, Parser)]
pub struct DoctorCli {
    /// Root of the configfs USB gadget to inspect.
    #[arg(long, value_name = "PATH", default_value = "/config/usb_gadget/g1")]
    gadget: PathBuf,

    /// Create and delete an unlinked UVC function instance to prove kernel
    /// support. Does not touch the active USB configuration.
    #[arg(long)]
    probe_function: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kernel_config() {
        let config = b"\
CONFIG_USB_CONFIGFS=y
# CONFIG_VIDEO_DEV is not set
CONFIG_USB_CONFIGFS_F_UVC=m
CONFIG_USB_CONFIGFS_F_UVC_EXTRA=y
";
        let parsed = parse_kernel_config(
            &config[..],
            &[
                "CONFIG_USB_CONFIGFS",
                "CONFIG_USB_CONFIGFS_F_UVC",
                "CONFIG_VIDEO_DEV",
                "CONFIG_MISSING",
            ],
        )
        .unwrap();

        assert_eq!(parsed["CONFIG_USB_CONFIGFS"], Tristate::Yes);
        assert_eq!(parsed["CONFIG_USB_CONFIGFS_F_UVC"], Tristate::Module);
        assert_eq!(parsed["CONFIG_VIDEO_DEV"], Tristate::No);
        assert_eq!(parsed["CONFIG_MISSING"], Tristate::No);
    }
}

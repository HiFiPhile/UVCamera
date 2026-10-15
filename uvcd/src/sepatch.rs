// SPDX-FileCopyrightText: 2024-2025 Andrew Gunnerson
// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only
// Based on msd-tool's sepatch from MSD.

//! Live SELinux policy patch: the `uvcamera_daemon` domain and who may talk
//! to it. Run at boot from `post-fs-data.sh` with `-ST`.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser};
use sepatch::{PolicyDb, RuleAction};

fn read_policy(path: &Path) -> Result<PolicyDb> {
    let data = fs::read(path).with_context(|| format!("Failed to open for reading: {path:?}"))?;

    let mut warnings = vec![];
    let pdb = PolicyDb::from_raw(&data, &mut warnings)
        .with_context(|| format!("Failed to parse sepolicy: {warnings:?}"))?;

    if !warnings.is_empty() {
        eprintln!("Warnings when loading sepolicy:");
        for warning in warnings {
            eprintln!("- {warning}");
        }
    }

    Ok(pdb)
}

fn write_policy(path: &Path, pdb: &PolicyDb) -> Result<()> {
    let mut warnings = vec![];
    let data = pdb
        .to_raw(&mut warnings)
        .with_context(|| format!("Failed to build sepolicy: {warnings:?}"))?;

    if !warnings.is_empty() {
        eprintln!("Warnings when saving sepolicy:");
        for warning in warnings {
            eprintln!("- {warning}");
        }
    }

    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("Failed to open for writing: {path:?}"))?;

    // Truncate only if needed. Some apps detect if the policy is modified
    // by looking at the modification timestamp of /sys/fs/selinux/load. A
    // write() syscall does not change mtime, but O_TRUNC does. Also, utimensat
    // does not work on selinuxfs.
    let metadata = file
        .metadata()
        .with_context(|| format!("Failed to stat file: {path:?}"))?;
    if metadata.len() > 0 {
        file.set_len(0)
            .with_context(|| format!("Failed to truncate file: {path:?}"))?;
    }

    let n = file
        .write(&data)
        .with_context(|| format!("Failed to write file: {path:?}"))?;
    if n != data.len() {
        bail!("Failed to write data in a single write call");
    }

    Ok(())
}

/// Name of the daemon's SELinux domain.
pub const DAEMON_TYPE: &str = "uvcamera_daemon";

/// Domains the root `app_process` supervisor may run in.
const SUPERVISOR_TYPES: [&str; 4] = ["ksu", "su", "magisk", "init"];

pub fn subcommand_sepatch(cli: &SepatchCli) -> Result<()> {
    let mut pdb = read_policy(cli.source.as_path())?;

    macro_rules! r {
        ($name:expr) => {{
            let name = $name;
            pdb.get_role_id(name)
                .ok_or_else(|| anyhow!("Role not found: {name}"))
        }};
    }
    macro_rules! t {
        ($name:expr) => {{
            let name = $name;
            pdb.get_type_id(name)
                .ok_or_else(|| anyhow!("Type not found: {name}"))
        }};
    }
    macro_rules! c {
        ($name:expr) => {{
            let name = $name;
            pdb.get_class_id(name)
                .ok_or_else(|| anyhow!("Class not found: {name}"))
        }};
    }
    macro_rules! allow {
        ($source:expr, $target:expr, $class:expr, [$($perm:expr),+ $(,)?]) => {{
            let (source, target, class) = ($source, $target, $class);
            for name in [$($perm),+] {
                let perm = pdb
                    .get_perm_id(class, name)
                    .ok_or_else(|| anyhow!("Permission not found in {class:?}: {name}"))?;
                pdb.set_rule(source, target, class, perm, RuleAction::Allow);
            }
        }};
    }
    macro_rules! dontaudit {
        ($source:expr, $target:expr, $class:expr, [$($perm:expr),+ $(,)?]) => {{
            let (source, target, class) = ($source, $target, $class);
            for name in [$($perm),+] {
                let perm = pdb
                    .get_perm_id(class, name)
                    .ok_or_else(|| anyhow!("Permission not found in {class:?}: {name}"))?;
                pdb.set_rule(source, target, class, perm, RuleAction::Deny);
            }
        }};
    }

    let c_capability = c!("capability")?;
    let c_chr_file = c!("chr_file")?;
    let c_dir = c!("dir")?;
    let c_fd = c!("fd")?;
    let c_fifo_file = c!("fifo_file")?;
    let c_file = c!("file")?;
    let c_filesystem = c!("filesystem")?;
    let c_lnk_file = c!("lnk_file")?;
    let c_process = c!("process")?;
    let c_sock_file = c!("sock_file")?;
    let c_unix_dgram_socket = c!("unix_dgram_socket")?;
    let c_unix_stream_socket = c!("unix_stream_socket")?;

    let t_app = t!("untrusted_app")?;
    let t_configfs = t!("configfs")?;
    let t_domain = t!("domain")?;
    let t_kernel = t!("kernel")?;
    let t_proc = t!("proc")?;
    let t_selinuxfs = t!("selinuxfs")?;
    let t_shell = t!("shell")?;
    let t_sysfs = t!("sysfs")?;
    let t_system_file = t!("system_file")?;
    let t_video_device = t!("video_device")?;

    // The daemon's domain.
    let t_daemon = pdb.create_type(DAEMON_TYPE, false)?.0;
    pdb.add_to_role(r!("r")?, t_daemon)?;
    pdb.set_attribute(t_daemon, t_domain, true)?;
    pdb.set_attribute(t_daemon, t!("mlstrustedsubject")?, true)?;

    // Entering the domain: the supervisor runs `runcon` on the module's
    // binary, which Magisk and KernelSU label system_file. The root
    // solutions' own domains may already transition anywhere; init is
    // allowed explicitly, as in MSD.
    allow!(
        t_daemon,
        t_system_file,
        c_file,
        ["entrypoint", "execute", "map", "read", "open", "getattr"]
    );
    let t_init = t!("init")?;
    allow!(
        t_init,
        t_daemon,
        c_process,
        ["transition", "rlimitinh", "siginh"]
    );

    // The root app_process supervisor exchanges authorization queries over
    // inherited pipes and is the daemon's parent.
    for name in SUPERVISOR_TYPES {
        if let Some(supervisor) = pdb.get_type_id(name) {
            allow!(t_daemon, supervisor, c_fd, ["use"]);
            allow!(t_daemon, supervisor, c_process, ["sigchld"]);
            allow!(
                t_daemon,
                supervisor,
                c_fifo_file,
                ["read", "write", "getattr"]
            );
        }
    }

    // Basics on itself: privilege drop, the IRQ boost, threads, signals and
    // its listening socket.
    allow!(
        t_daemon,
        t_daemon,
        c_capability,
        ["chown", "setuid", "setgid", "sys_nice"]
    );
    // Only used where the gadget belongs to root (see daemon.rs).
    allow!(t_daemon, t_daemon, c_capability, ["dac_override"]);
    allow!(
        t_daemon,
        t_daemon,
        c_process,
        [
            "fork", "sigchld", "signal", "getsched", "setsched", "getcap", "setcap"
        ]
    );
    allow!(
        t_daemon,
        t_daemon,
        c_unix_stream_socket,
        // `connect` lets the policy self-check reach the `connectto` denial.
        [
            "create", "bind", "connect", "listen", "accept", "read", "write", "getattr", "getopt",
            "setopt", "shutdown"
        ]
    );

    // Logging to logcat.
    if let (Some(t_logd), Some(t_logdw_socket)) =
        (pdb.get_type_id("logd"), pdb.get_type_id("logdw_socket"))
    {
        allow!(
            t_daemon,
            t_daemon,
            c_unix_dgram_socket,
            ["create", "connect", "write", "getopt", "setopt"]
        );
        allow!(t_daemon, t_logd, c_unix_dgram_socket, ["sendto"]);
        allow!(t_daemon, t_logdw_socket, c_sock_file, ["write"]);
    }

    // SELinux status check (reading enforce, statfs of selinuxfs).
    allow!(t_daemon, t_selinuxfs, c_file, ["open", "read", "getattr"]);
    allow!(t_daemon, t_selinuxfs, c_filesystem, ["getattr"]);

    // configfs gadget manipulation. Samsung uses its own type.
    let mut configfs_types = vec![t_configfs];
    configfs_types.extend(pdb.get_type_id("usb_configfs"));
    for target in configfs_types {
        allow!(
            t_daemon,
            target,
            c_dir,
            [
                "add_name",
                "create",
                "getattr",
                "open",
                "read",
                "remove_name",
                "rmdir",
                "search",
                "setattr",
                "write"
            ]
        );
        allow!(
            t_daemon,
            target,
            c_file,
            ["create", "getattr", "open", "read", "setattr", "write"]
        );
        allow!(
            t_daemon,
            target,
            c_lnk_file,
            ["create", "getattr", "read", "setattr", "unlink"]
        );
    }

    // The gadget's video node.
    allow!(
        t_daemon,
        t_video_device,
        c_chr_file,
        ["getattr", "ioctl", "map", "open", "read", "write"]
    );

    // Finding the video node and the controller's driver in sysfs.
    let mut sysfs_types = vec![t_sysfs];
    sysfs_types.extend(
        ["sysfs_udc", "sysfs_video"]
            .iter()
            .filter_map(|n| pdb.get_type_id(*n)),
    );
    for target in sysfs_types {
        allow!(
            t_daemon,
            target,
            c_dir,
            ["getattr", "open", "read", "search"]
        );
        allow!(t_daemon, target, c_file, ["getattr", "open", "read"]);
        allow!(t_daemon, target, c_lnk_file, ["getattr", "read"]);
    }

    // Finding the controller's IRQ thread: list /proc and read the names of
    // kernel threads, then change their scheduling policy. Other processes
    // are skipped silently.
    allow!(
        t_daemon,
        t_proc,
        c_dir,
        ["getattr", "open", "read", "search"]
    );
    allow!(t_daemon, t_kernel, c_dir, ["getattr", "search"]);
    allow!(t_daemon, t_kernel, c_file, ["getattr", "open", "read"]);
    allow!(t_daemon, t_kernel, c_process, ["getsched", "setsched"]);
    dontaudit!(t_daemon, t_domain, c_dir, ["getattr", "search"]);
    dontaudit!(t_daemon, t_domain, c_file, ["getattr", "open", "read"]);

    // Block the daemon from connecting to itself. The daemon uses this to
    // check that the policy is loaded.
    dontaudit!(t_daemon, t_daemon, c_unix_stream_socket, ["connectto"]);

    // Apps may connect. The daemon authorizes the peer UID (package name and
    // signing certificate) before reading anything from it.
    allow!(t_app, t_daemon, c_unix_stream_socket, ["connectto"]);

    // `uvcd client` from an adb shell is rejected by default; root shells
    // run in the root solution's domain.
    if cli.allow_adb {
        allow!(t_shell, t_daemon, c_unix_stream_socket, ["connectto"]);
    }

    // Set either way: the source is often the live policy, which may already
    // contain our domain from an earlier run.
    pdb.set_permissive(t_daemon, cli.permissive);

    if cli.strip_no_audit {
        pdb.strip_no_audit();
    }

    write_policy(cli.target.as_path(), &pdb)?;

    Ok(())
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct SourceGroup {
    /// Source policy file.
    #[arg(short, long, value_parser, value_name = "FILE")]
    source: Option<PathBuf>,

    /// Use currently loaded policy as source.
    #[arg(short = 'S', long)]
    source_kernel: bool,
}

impl SourceGroup {
    fn as_path(&self) -> &Path {
        if let Some(path) = &self.source {
            path
        } else if self.source_kernel {
            Path::new("/sys/fs/selinux/policy")
        } else {
            unreachable!()
        }
    }
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct TargetGroup {
    /// Target policy file.
    #[arg(short, long, value_parser, value_name = "FILE")]
    target: Option<PathBuf>,

    /// Load patched policy into kernel.
    #[arg(short = 'T', long)]
    target_kernel: bool,
}

impl TargetGroup {
    fn as_path(&self) -> &Path {
        if let Some(path) = &self.target {
            path
        } else if self.target_kernel {
            Path::new("/sys/fs/selinux/load")
        } else {
            unreachable!()
        }
    }
}

/// Patch SELinux policy file.
#[derive(Debug, Parser)]
pub struct SepatchCli {
    #[command(flatten)]
    source: SourceGroup,

    #[command(flatten)]
    target: TargetGroup,

    /// Remove dontaudit/dontauditxperm rules.
    #[arg(short = 'd', long)]
    strip_no_audit: bool,

    /// Allow connections from adb shell session.
    #[arg(long)]
    allow_adb: bool,

    /// Make the daemon's domain permissive (log denials without enforcing
    /// them). Only for developing the policy.
    #[arg(long)]
    permissive: bool,
}

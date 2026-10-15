// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! The usb_f_uvcx kernel module (see `kmod/`): the upstream UVC gadget
//! function with frame-based formats and bulk streaming. One build per GKI
//! kernel module interface ships next to the daemon, named after it, e.g.
//! `usb_f_uvcx.android16-6.12-6.ko`; it is loaded when the app asks for it.
//! The daemon opens it while still root (the module directory is under
//! /data/adb, which only root can enter) and keeps the descriptor.

use std::{
    fs::File,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use tracing::info;

use crate::message::ModuleState;

pub const MODULE_NAME: &str = "usb_f_uvcx";

/// The kernel module interface of a GKI kernel release such as
/// `6.12.58-android16-6-g925a103d123c-ab15898589-4k`: `android16-6.12-6`
/// (Android release, kernel version, KMI generation). Modules built for one
/// load on every kernel with the same interface.
pub fn kmi(release: &str) -> Option<String> {
    let mut parts = release.split('-');
    let version = parts.next()?;
    let android = parts.next().filter(|p| p.starts_with("android"))?;
    let generation = parts.next().filter(|p| p.parse::<u32>().is_ok())?;
    let mut numbers = version.split('.');
    let (major, minor) = (numbers.next()?, numbers.next()?);
    Some(format!("{android}-{major}.{minor}-{generation}"))
}

fn kernel_release() -> String {
    rustix::system::uname()
        .release()
        .to_string_lossy()
        .into_owned()
}

/// The module built for the running kernel, opened, if `dir` has one.
pub struct Module {
    path: PathBuf,
    file: File,
}

impl Module {
    pub fn open(dir: &Path) -> Option<Self> {
        let kmi = kmi(&kernel_release())?;
        let path = dir.join(format!("{MODULE_NAME}.{kmi}.ko"));
        let file = File::open(&path).ok()?;
        info!("Found {path:?}");
        Some(Self { path, file })
    }
}

pub fn is_loaded() -> bool {
    Path::new("/sys/module").join(MODULE_NAME).is_dir()
}

pub fn state(module: Option<&Module>) -> ModuleState {
    if is_loaded() {
        ModuleState::Loaded
    } else if module.is_some() {
        ModuleState::Available
    } else {
        ModuleState::Unavailable
    }
}

/// Load the module unless it is loaded already.
pub fn load(module: Option<&Module>) -> Result<()> {
    if is_loaded() {
        return Ok(());
    }
    let Some(Module { path, file }) = module else {
        bail!("No {MODULE_NAME} module for kernel {}", kernel_release());
    };

    // SAFETY: finit_module only reads the file and the empty parameter string.
    let ret = unsafe { libc::syscall(libc::SYS_finit_module, file.as_raw_fd(), c"".as_ptr(), 0) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EEXIST) {
            return Err(err).with_context(|| format!("Failed to load {path:?}"));
        }
    }
    info!("Loaded {path:?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kmi() {
        assert_eq!(
            kmi("6.12.58-android16-6-g925a103d123c-ab15898589-4k").as_deref(),
            Some("android16-6.12-6")
        );
        assert_eq!(
            kmi("6.1.75-android14-11-gabcdef").as_deref(),
            Some("android14-6.1-11")
        );
        assert_eq!(kmi("6.8.0-45-generic"), None);
        assert_eq!(kmi("5.10.0"), None);
    }
}

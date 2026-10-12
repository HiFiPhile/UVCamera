// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! Real-time priority for the USB controller's threaded IRQ handler while
//! streaming.
//!
//! Isochronous transfers need the controller's completion interrupts to be
//! serviced within a few milliseconds, or f_uvc runs out of queued requests,
//! the controller misses service intervals, and the rest of the frame is
//! dropped. Some vendor kernels (e.g. Qualcomm's dwc3) explicitly run that
//! IRQ thread as SCHED_NORMAL, which on a busy phone dropped ~6–20% of
//! 600 KB YUYV frames. SCHED_FIFO eliminates it.
//!
//! The thread is recreated every time the gadget is bound, so the boost is
//! applied at each stream start and the original policy restored at stop.

use std::{fs, io, path::Path};

use tracing::{debug, info, warn};

/// The kernel's default for threaded IRQ handlers, i.e. what the vendor
/// driver opted out of. Other IRQ threads at this level still take turns.
const RT_PRIORITY: i32 = 50;

struct Saved {
    pid: libc::pid_t,
    name: String,
    policy: i32,
    priority: i32,
}

/// Boosts the IRQ threads of one USB device controller.
pub struct IrqBoost {
    /// IRQ action name, i.e. the suffix of the `irq/<n>-<name>` thread.
    action: String,
    saved: Vec<Saved>,
}

impl IrqBoost {
    /// `udc` is the controller name, e.g. `a600000.dwc3`.
    pub fn new(udc: &str) -> Self {
        Self {
            action: irq_action_name(udc),
            saved: vec![],
        }
    }

    /// Switch the controller's IRQ threads to SCHED_FIFO.
    pub fn apply(&mut self) {
        self.restore();

        let threads = find_irq_threads(&self.action);
        if threads.is_empty() {
            warn!("No IRQ thread found for USB controller ({:?})", self.action);
            return;
        }

        for (pid, name) in threads {
            // SAFETY: Querying scheduling attributes of another process.
            let policy = unsafe { libc::sched_getscheduler(pid) };
            let mut param = libc::sched_param { sched_priority: 0 };
            if policy < 0 || unsafe { libc::sched_getparam(pid, &mut param) } < 0 {
                warn!(
                    "Failed to query scheduling of {name}: {}",
                    io::Error::last_os_error()
                );
                continue;
            }

            let boosted = libc::sched_param {
                sched_priority: RT_PRIORITY,
            };
            // SAFETY: Changing scheduling attributes of another process.
            if unsafe { libc::sched_setscheduler(pid, libc::SCHED_FIFO, &boosted) } < 0 {
                warn!(
                    "Failed to set SCHED_FIFO on {name}: {}",
                    io::Error::last_os_error()
                );
                continue;
            }

            info!("Raised {name} (pid {pid}) to SCHED_FIFO {RT_PRIORITY}");
            self.saved.push(Saved {
                pid,
                name,
                policy,
                priority: param.sched_priority,
            });
        }
    }

    /// Put the threads back to their original policy.
    pub fn restore(&mut self) {
        for saved in self.saved.drain(..) {
            let param = libc::sched_param {
                sched_priority: saved.priority,
            };
            // The thread may be gone if the gadget was unbound meanwhile.
            // SAFETY: Changing scheduling attributes of another process.
            if unsafe { libc::sched_setscheduler(saved.pid, saved.policy, &param) } < 0 {
                debug!(
                    "Failed to restore {}: {}",
                    saved.name,
                    io::Error::last_os_error()
                );
            } else {
                debug!("Restored scheduling of {}", saved.name);
            }
        }
    }
}

impl Drop for IrqBoost {
    fn drop(&mut self) {
        self.restore();
    }
}

/// The IRQ action name is the controller's driver name for the common UDC
/// drivers (`dwc3`, `dwc2`, `cdns3`, `musb-hdrc`, …).
fn irq_action_name(udc: &str) -> String {
    let link = Path::new("/sys/class/udc").join(udc).join("device/driver");
    fs::read_link(link)
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "dwc3".to_owned())
}

/// Kernel threads named `irq/<n>-<action>`. Thread names are truncated to
/// 15 characters, so compare against the truncated form.
fn find_irq_threads(action: &str) -> Vec<(libc::pid_t, String)> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return vec![];
    };

    entries
        .flatten()
        .filter_map(|e| {
            let pid = e.file_name().to_str()?.parse::<libc::pid_t>().ok()?;
            let comm = fs::read_to_string(e.path().join("comm")).ok()?;
            let comm = comm.trim_end();
            is_irq_thread_for(comm, action).then(|| (pid, comm.to_owned()))
        })
        .collect()
}

fn is_irq_thread_for(comm: &str, action: &str) -> bool {
    let Some(rest) = comm.strip_prefix("irq/") else {
        return false;
    };
    let Some((number, name)) = rest.split_once('-') else {
        return false;
    };
    !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
        && !name.is_empty()
        && action.starts_with(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_irq_thread_names() {
        assert!(is_irq_thread_for("irq/332-dwc3", "dwc3"));
        // Truncated to 15 characters by the kernel.
        assert!(is_irq_thread_for("irq/120-musb-hd", "musb-hdrc"));
        assert!(!is_irq_thread_for("irq/332-dwc3-otg", "dwc3"));
        assert!(!is_irq_thread_for("irq/x-dwc3", "dwc3"));
        assert!(!is_irq_thread_for("kworker/u33:4", "dwc3"));
        assert!(!is_irq_thread_for("irq/332-", "dwc3"));
    }
}

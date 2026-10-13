# SPDX-FileCopyrightText: 2026 HiFiPhile
# SPDX-License-Identifier: GPL-3.0-only
# Runs early in boot, before services start: add the daemon's SELinux domain
# to the live policy.

mod_dir=${0%/*}
log_dir=/data/local/tmp/uvcamera

mkdir -p "${log_dir}"
exec >"${log_dir}/post-fs-data.log" 2>&1

echo "Timestamp: $(date)"
"${mod_dir}/uvcd.$(getprop ro.product.cpu.abi)" sepatch -ST
echo "uvcd sepatch exit status: ${?}"

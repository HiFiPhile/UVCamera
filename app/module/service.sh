# SPDX-FileCopyrightText: 2026 HiFiPhile
# SPDX-License-Identifier: GPL-3.0-only
# Runs at late_start service. Logs go to /data/local/tmp/uvcamera.

mod_dir=${0%/*}
log_dir=/data/local/tmp/uvcamera

mkdir -p "${log_dir}"
exec >"${log_dir}/service.log" 2>&1

header() {
    echo "----- ${*} -----"
}

module_prop() {
    grep "^${1}=" "${mod_dir}/module.prop" | cut -d= -f2
}

app_id=$(module_prop id)
module_version_code=$(module_prop versionCode)
uvcd="${mod_dir}/uvcd.$(getprop ro.product.cpu.abi)"

header Environment
echo "Timestamp: $(date)"
echo "App ID: ${app_id}"
echo "Module version: $(module_prop version) (${module_version_code})"
echo "UID/GID/Context: $(id)"

header Waiting for boot
until [ "$(getprop sys.boot_completed)" = 1 ]; do
    sleep 1
done

# The app is a regular user app, not a system app, so it does not depend on the
# root solution's module mounting. Install or upgrade it from the module.
header Installing app
installed_version_code=$(dumpsys package "${app_id}" \
    | sed -n 's/.*versionCode=\([0-9]*\).*/\1/p' | head -n 1)
echo "Installed version code: ${installed_version_code:-<none>}"

if [ -z "${installed_version_code}" ] \
        || [ "${installed_version_code}" -lt "${module_version_code}" ]; then
    # system_server cannot read files under /data/adb, so stage the APK.
    apk="${log_dir}/app.apk"
    cp "${mod_dir}/UVCamera.apk" "${apk}" && pm install -r "${apk}"
    echo "pm install exit status: ${?}"
    rm -f "${apk}"
fi

header Device report
"${uvcd}" doctor --probe-function > "${log_dir}/doctor.log" 2>&1
echo "uvcd doctor exit status: ${?}"

# The supervisor runs as root, starts uvcd in the uvcamera_daemon SELinux
# domain, and tells it which client UIDs belong to the UVCamera app signed
# with the pinned certificate. Daemon logs go to logcat (tag uvcd).
header Starting daemon
CLASSPATH="${mod_dir}/UVCamera.apk" /system/bin/app_process / \
    com.hifiphile.uvcamera.standalone.AuthSupervisor \
    "${uvcd}" "${mod_dir}/client-cert.sha256" &
echo "Supervisor pid: ${!}"

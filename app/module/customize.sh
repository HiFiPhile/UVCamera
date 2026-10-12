# SPDX-FileCopyrightText: 2023-2024 Andrew Gunnerson
# SPDX-FileCopyrightText: 2026 HiFiPhile
# SPDX-License-Identifier: GPL-3.0-only
# Sourced by the Magisk/KernelSU installer.

abi=$(getprop ro.product.cpu.abi)

echo "ABI: ${abi}"

run() {
    if [ ! -f "${MODPATH}/uvcd.${abi}" ]; then
        echo "Unsupported ABI: ${abi}"
        return 1
    fi

    echo 'Making uvcd executable'
    chmod -v 0755 "${MODPATH}/uvcd.${abi}" 2>&1
}

if ! run 2>&1; then
    rm -rv "${MODPATH}" 2>&1
    exit 1
fi

#!/bin/sh
# SPDX-FileCopyrightText: 2026 HiFiPhile
# SPDX-License-Identifier: GPL-3.0-only
# Build usb_f_uvcx against the GKI kernel set up as described in README.md,
# into out/usb_f_uvcx.<KMI>.ko.
set -eu

. "${GKI_ENV:-${HOME}/Android/gki/env.sh}"
: "${GKI_KMI:?GKI_KMI (e.g. android16-6.12-6) is not set}"

cd "$(dirname "$0")"
# shellcheck disable=SC2086 # GKI_MAKE_ARGS is a list of arguments.
make ${GKI_MAKE_ARGS} M="${PWD}" modules
mkdir -p out
llvm-strip --strip-debug -o "out/usb_f_uvcx.${GKI_KMI}.ko" usb_f_uvcx.ko
echo "Built out/usb_f_uvcx.${GKI_KMI}.ko"

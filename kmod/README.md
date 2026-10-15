# usb_f_uvcx

The Linux UVC gadget function (`drivers/usb/gadget/function/*uvc*`) from
upstream, built as an out-of-tree module that registers itself as `uvcx`.
It sits next to a kernel's built-in `uvc` function and brings what older
kernels lack:

- frame-based formats (H.264, Linux 6.13+), and
- `compat.h`: shims for the kernel APIs that changed since 6.12.

Source: torvalds/linux 045958acaa75 (v7.3-rc7). Licensed GPL-2.0, like the
kernel it is part of.

## Building for the OnePlus 15 (GKI android16-6.12)

The module must match the kernel's KMI: build it against the same
`kernel/common` commit, configuration and clang as the phone's kernel, with
that build's `vmlinux.symvers`.

```bash
. ~/Android/gki/env.sh
make $GKI_MAKE_ARGS M=$PWD modules
```

`~/Android/gki` holds `kernel/common` at 925a103d123c (from
`/proc/version`), clang r536225, the phone's `/proc/config.gz` as
`out/.config` (`make olddefconfig modules_prepare`), and the GKI build's
`vmlinux.symvers` as `out/Module.symvers` (ci.android.com build 15898589,
`kernel_aarch64`). `out/protected_module_names_list` is an empty file:
the module exports no symbols.

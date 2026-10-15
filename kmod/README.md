# usb_f_uvcx

The Linux UVC gadget function (`drivers/usb/gadget/function/*uvc*`) from
upstream, built as an out-of-tree module that registers itself as `uvcx`.
It sits next to a kernel's built-in `uvc` function and brings what older
kernels lack:

- frame-based formats (H.264, Linux 6.13+),
- bulk streaming (not upstream; after Jing Leng's unmerged 2022 series), and
- `compat.h`: shims for the kernel APIs that changed since 6.12.

## Bulk streaming

```bash
echo bulk > functions/uvcx.NAME/streaming_transfer
echo $((512 * 1024)) > functions/uvcx.NAME/streaming_maxpacket
```

`streaming_maxpacket` is then the payload size (dwMaxPayloadTransferSize,
header included, rounded up to 1024, at most 16 MiB), which userspace must
report in its probe/commit responses. The streaming interface has only
alternate setting 0, with the endpoint, which stays enabled while the
configuration is.

A stream starts with SET_CUR(VS_COMMIT_CONTROL): the function queues
UVC_EVENT_STREAMON after the commit's data. Hosts stop a bulk stream with
CLEAR_FEATURE(ENDPOINT_HALT), which the UDC handles without telling the
function; it is only seen when it cancels a request in flight. So the
function also sends UVC_EVENT_STREAMOFF when the host hasn't read anything
for 2 s, and a COMMIT during a stream sends STREAMOFF, then STREAMON once
userspace has turned the stream off. Requests still queued at STREAMOFF are
dequeued, so a stream never starts with the previous one's data.

Bulk mode uses vmalloc buffers: the bulk encoder copies from the buffer's
kernel mapping, which scatter-gather buffers lack.

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

# UVCamera architecture

UVCamera turns a rooted Android phone into a standard USB webcam (USB Video
Class). The phone appears to the host as a UVC device next to ADB/MTP, and the
host uses its stock driver (`uvcvideo` on Linux, `usbvideo.sys` on Windows,
the built-in UVC support on macOS). No host software is needed.

Requirements on the phone:

- Root via **Magisk or KernelSU**. SELinux stays **enforcing**.
- A configfs USB gadget at `/config/usb_gadget/g1`. This is the AOSP default
  since Android 8 or so.
- A kernel with the **UVC gadget function** (`CONFIG_USB_CONFIGFS_F_UVC`).
  Android 14+ GKI kernels usually have it because of the AOSP
  "DeviceAsWebcam" feature. `uvcd doctor --probe-function` checks it
  definitively.
- Android 11+ (API 30), the same baseline as MSD.

## Relationship to MSD

The design follows [MSD](https://github.com/HiFiPhile/MSD): a small Rust
daemon, started by a root module, owns the privileged USB gadget operations.
An unprivileged app drives it over an authenticated abstract Unix socket.
These pieces carry over from MSD nearly unchanged:

| MSD piece | Reused for |
|---|---|
| `usb.rs` configfs helpers (fs-magic checks, chown-on-create, symlink config) | gadget setup |
| Gadget HAL `SIGSTOP` while reconfiguring, `sys.usb.controller` for the UDC | gadget setup |
| `sepatch` crate + live policy patch in `post-fs-data.sh` | SELinux |
| `AuthenticatedDaemon` (app_process root supervisor, UID → package → signing cert pin) | IPC auth |
| Versioned binary protocol, `uvcd client` debug CLI | IPC |
| Gradle → `cargo android` per-ABI builds, module ZIP packaging | build |

The fundamental difference: **mass storage is fire-and-forget**. The kernel
serves block I/O from a file once configured, so MSD's daemon only acts on
request. **UVC is a live data path.** Userspace must:

1. Answer UVC class control requests (`PROBE`/`COMMIT`, camera/processing
   unit controls) forwarded by the kernel as V4L2 events.
2. Continuously feed encoded frames into the V4L2 output queue of
   `/dev/videoN` while the host streams.

The camera can only be used from an app process, through Camera2. So frames
are produced in the app and handed to the daemon, which owns the V4L2 device.

## Components

```
 USB host  ── stock UVC driver (uvcvideo / usbvideo.sys / macOS)
    ▲   EP0 control (class requests)      isochronous video EP
────┼──────────────────────────── kernel ───────────────────────────────────
    UDC (dwc3, …)  ◄── configfs gadget g1, config b.1:
                        ffs.adb, mtp, … (owned by gadget HAL)
                        uvc.uvcamera            (owned by uvcd)
                          │
                       f_uvc ── /dev/videoN  (V4L2 OUTPUT queue + UVC events)
────┼──────────────────────────── userspace ────────────────────────────────
    │
  uvcd daemon  (Rust; SELinux domain uvcamera_daemon; uid system + CAP_CHOWN)
    ├─ gadget/   build uvc.uvcamera from a FormatSpec, link into b.1,
    │            pause gadget HAL, rebind UDC, tear down
    ├─ v4l2/     ioctl bindings: SUBSCRIBE_EVENT, DQEVENT, S_FMT,
    │            REQBUFS/QUERYBUF/mmap, QBUF/DQBUF, STREAMON/OFF,
    │            UVCIOC_SEND_RESPONSE
    ├─ uvc/      pure protocol logic (host-testable): PROBE/COMMIT
    │            negotiation, control request routing, error codes
    └─ ipc/      abstract socket "uvcamerad"; control + frame streams
            ▲
            │  authorized per request by the root supervisor
            │  (UID → package name → signing certificate pin)
            ▼
  UVCamera app  (com.hifiphile.uvcamera; untrusted_app; normal user app)
    ├─ WebcamService   foreground service (type=camera); owns the session
    ├─ daemon/Client   protocol client (mirrors uvcd's ipc/message)
    ├─ camera/         Camera2 source; capability discovery → FormatSpec
    ├─ encoder/        HW JPEG | libjpeg-turbo MJPEG | YUYV | MediaCodec H.264
    └─ ui/             Compose: status, camera + format choice, controls
```

### Root module (`app/module/`)

| Script | Stage | Job |
|---|---|---|
| `customize.sh` | install | `chmod` the ABI-specific `uvcd` |
| `post-fs-data.sh` | early boot | `uvcd sepatch -ST`: patch the live SELinux policy *(Phase 2)* |
| `service.sh` | late start | wait for boot, install/upgrade the bundled APK with `pm install`, write `uvcd doctor` report, start the supervisor *(Phase 2)* |

The module ships the APK, but the app is installed as a **regular user app**.
It is not mounted into `/system/priv-app`. It needs no privileged permissions,
and this avoids the Hybrid Mount / "umount modules" / launcher visibility
problems that MSD-KSU has to document for KernelSU. The supervisor still uses
the module's copy of the APK as the `app_process` classpath, exactly like MSD.

Logs go to `/data/local/tmp/uvcamera/` (`service.log`, `doctor.log`).

### `uvcd` (Rust, `uvcd/`)

A single static binary per ABI with subcommands:

| Subcommand | Phase | Purpose |
|---|---|---|
| `doctor [--probe-function]` | 0 ✅ | Read-only device capability report |
| `gadget up/down [--test-pattern]` | 1 | Root-only bring-up without the app |
| `sepatch` | 2 | Live SELinux policy patch |
| `daemon` | 2 | Long-running service behind the supervisor |
| `client …` | 2 | Debug CLI speaking the app protocol |

Process model (Phase 2+, from MSD): `service.sh` → `app_process` runs
`standalone.AuthSupervisor` as root. The supervisor spawns
`runcon u:r:uvcamera_daemon:s0 uvcd daemon` with its stdin/stdout as a private
auth pipe. The daemon drops to `system:system`, keeping only `CAP_CHOWN`.
configfs creates new nodes as `root:root`, so it must `chown` them. It listens
on `@uvcamerad` and asks the supervisor to authorize every peer UID, per
request.

Event loop: one thread `poll()`s `/dev/videoN` (V4L2 events + `POLLOUT` for
buffer completion), the client sockets, and a self-pipe for shutdown. The
UVC protocol state is a plain struct, `uvc::Session`. It consumes
`UVC_EVENT_*` and returns actions such as "send response" or "start/stop
stream". That keeps it unit-testable on the host without a kernel.

### App (Kotlin + Compose, `app/`)

- **WebcamService**: foreground service with
  `foregroundServiceType="camera"`. It must be started while the activity is
  visible, because Android 11+ denies camera access to an FGS started from the
  background. It then stays alive while webcam mode is enabled, holds the
  daemon connection, and opens the camera only between the daemon's
  `StreamStart` and `StreamStop`. The privacy indicator is therefore on only
  while the host is actually streaming.
- **camera/**: queries Camera2 capabilities (done in Phase 0) to build the
  `FormatSpec` to advertise. It opens the selected camera with an
  `ImageReader` in the needed format and pins the AE target FPS range to the
  committed frame interval.
- **encoder/**: one interface, `FrameEncoder(format, size, fps) → frames`,
  with strategies chosen per device:

  | Strategy | Output | Cost | Notes |
  |---|---|---|---|
  | Camera2 `JPEG` stream | MJPEG | ~0 CPU (ISP/HW) | Only if `minFrameDuration(JPEG, size)` ≤ 33.3 ms. Phase 0 screen reports this. Quality/latency to be verified. |
  | `YUV_420_888` → libjpeg-turbo (NEON, JNI) | MJPEG | 1 core @1080p30 | Universal fallback |
  | `YUV_420_888` → YUYV repack | Uncompressed | memcpy | USB 2.0: 640×480@30 max |
  | `YUV_420_888`/Surface → MediaCodec AVC | H.264 (UVC 1.5 frame-based) | HW | Needs `streaming/framebased` in configfs; Phase 5 |

## Session lifecycle

```
 App                        uvcd                         Kernel / Host
  │ user taps Enable         │                              │
  │ start WebcamService      │                              │
  │── connect @uvcamerad ───►│ authorize UID via supervisor │
  │── Configure(FormatSpec) ►│ pause HAL, UDC="",           │
  │                          │ mkdir functions/uvc.uvcamera,│
  │                          │ write descriptors, link b.1, │
  │                          │ set bDeviceClass EF/02/01,   │
  │                          │ UDC=<controller>, resume HAL │
  │                          │ open /dev/videoN (function_name match)
  │                          │◄──────── UVC_EVENT_CONNECT ──│ host enumerates
  │                          │◄─ SETUP/DATA: PROBE, COMMIT ─│ host opens camera
  │                          │── UVCIOC_SEND_RESPONSE ─────►│
  │                          │◄──────── UVC_EVENT_STREAMON ─│
  │                          │ S_FMT, REQBUFS(mmap), STREAMON
  │◄─ StreamStart(fmt,w,h,ival)                             │
  │ open camera, encode      │                              │
  │── Frame(bytes, pts) ────►│ DQBUF free → copy → QBUF ───►│ isoc to host
  │   … (newest wins when no free buffer)                   │
  │                          │◄──────── UVC_EVENT_STREAMOFF │ host closes
  │◄─ StreamStop             │ STREAMOFF, REQBUFS(0)        │
  │ close camera             │                              │
  │── Disable ──────────────►│ unlink + rmdir function,     │
  │                          │ restore device class, rebind │
```

If the app or its connection dies while the function is linked, the daemon
keeps the gadget up and feeds a "camera unavailable" placeholder frame. When
the app reconnects, it resumes. An explicit Disable, or a reboot, removes the
function.

## Format advertisement

UVC descriptors are fixed at enumeration, so the advertised formats must be
decided **before** the UDC is bound. Changing them means a re-enumeration,
which drops ADB for about a second. The app derives the `FormatSpec` from the
selected camera's streamable sizes (≥ 30 fps), clipped to bandwidth:

| Format | Default frames | Bandwidth (USB 2.0 HS isoc, ~24 MB/s theoretical) |
|---|---|---|
| MJPEG | 1920×1080, 1280×720, 640×480 @ 30/15 fps | 1080p30 ≈ 3–9 MB/s, fine |
| YUYV | 640×480 @ 30, 1280×720 @ 10 | 640×480×2×30 = 18.4 MB/s, at the limit |
| H.264 (frame-based) | as MJPEG | ≪ 1 MB/s; Phase 5 |

`streaming_maxpacket=3072` (HS high-bandwidth, 3×1024) and, on SuperSpeed
UDCs, `streaming_maxburst` raise isochronous throughput. On pre-6.13 kernels,
larger bursts also shrink the timing margin, so uvcd uses the smallest burst
that fits. While streaming, it raises the USB controller's IRQ thread to
SCHED_FIFO; without that, the phone dropped up to 6% of YUYV frames (see
PLAN.md, "Isochronous reliability"). `uvcd` derives
`dwMaxVideoFrameBufferSize` and bitrates itself and keeps its own format
table. It does not depend on the kernel's `VIDIOC_ENUM_FMT` support, which
only newer kernels have.

## IPC protocol

Abstract Unix stream socket `@uvcamerad`. It is versioned and length-prefixed,
in MSD's style: a protocol-version handshake, then tagged messages. Malformed
input closes the connection.

- **Control (app → daemon):** `GetStatus`, `Configure(FormatSpec)`,
  `Disable`, `ControlValue(id, value)`.
- **Control (daemon → app):** `Status`, `StreamStart(format, w, h,
  interval)`, `StreamStop`, `ControlRequest(id, value)` (host changed
  zoom/brightness), `Error`.
- **Frames (app → daemon):** `Frame(pts_us, len, bytes)` on the same
  connection. MJPEG at 1080p30 is about 10 MB/s, trivial for a Unix socket.

The daemon copies each frame into a mmap'd V4L2 buffer. If profiling shows the
copy matters (YUYV, high resolutions), Phase 4 replaces inline frames with a
memfd ring passed via `SCM_RIGHTS`, plus `V4L2_MEMORY_USERPTR`/`DMABUF`. The
uvc queue supports `VB2_MMAP | VB2_USERPTR | VB2_DMABUF`.

## SELinux

New domain `uvcamera_daemon`, created by `uvcd sepatch`, same pattern as
MSD's `msd_daemon`. Minimum rules:

- `configfs`: dir `{create search add_name remove_name rmdir read open write
  setattr}`, file `{create open read write getattr setattr}`, lnk_file
  `{create read unlink setattr}`
- `video_device:chr_file {open read write ioctl getattr map}` + ioctl xperms
- `capability {chown setuid setgid}`, `process {signal sigstop}` on the gadget
  HAL domain (`hal_usb_gadget_default`)
- read `usb_control_prop` (`sys.usb.controller`), sysfs (`/sys/class/udc`,
  `video4linux`)
- `capability sys_nice` and `process {getsched setsched}` on `kernel` (the
  controller's IRQ thread), plus `/proc/<pid>/comm` read for kernel threads
- `untrusted_app → uvcamera_daemon:unix_stream_socket connectto`
- **Deny self-connect.** The daemon refuses to serve unless SELinux is
  enforcing and a connection to itself fails. This proves the policy is loaded
  (from MSD).

App domain rules are not widened. The app never touches `/dev/video*` or
configfs.

## Security model

- Only the signed UVCamera app may drive the daemon. The pin is the SHA-256 of
  the signing certificate, packaged into the module at build time. It is
  checked per request by the root supervisor through PackageManager.
- The daemon cannot open files or devices on behalf of clients. The only
  effects available to a client are building and removing **its own**
  `uvc.uvcamera` function and pushing frame bytes.
- The camera is opened only by the app through public Camera2 APIs, so the
  OS privacy indicator and camera-access policy stay in force.

## Coexistence and edge cases

- **ADB/MTP keep working.** UVC is an extra function in `b.1`. The device
  class becomes Misc/IAD (`0xEF/0x02/0x01`) so Windows binds the UVC
  interface association correctly. The original values are restored on
  disable.
- **Gadget HAL:** `SIGSTOP`ped while reconfiguring (MSD's approach), then
  resumed. If the user changes the USB mode in Settings, the HAL may unlink
  our function. The daemon watches for this (inotify on `configs/b.1` / UDC
  state) and re-applies *(Phase 4)*. Devices without a gadget HAL
  (`vendor.usb.use_gadget_hal=0`, e.g. Qualcomm/OnePlus) reconfigure through
  init property triggers instead. Those only remove the `f1…f9` links, so a
  uniquely named link survives mode switches. See the device notes in
  PLAN.md.
- **MSD side by side:** both link distinct names into `b.1` and pause the HAL
  the same way. This should compose but needs testing.
- **Camera contention:** if another app takes the camera
  (`onDisconnected`), the app sends a placeholder frame and retries.
- **OEM DeviceAsWebcam present** (`uvc.*` already in configfs, or
  `ro.usb.uvc.enabled=true`): detected by `doctor`. We use our own function
  name and never touch theirs.

## Alternatives considered

1. **Enable the stock AOSP DeviceAsWebcam** (`ro.usb.uvc.enabled`). Needs
   gadget HAL support for `GadgetFunction.UVC` and the platform-signed
   DeviceAsWebcam service. Few non-Pixel devices have either, and root cannot
   add platform signatures. Rejected as the main path. `doctor` still reports
   it.
2. **Hand `/dev/videoN` to the app via fd passing** and run UVC in JNI.
   Rejected: it needs `untrusted_app` rules on `video_device`, which applies
   to every app on the device, and it moves protocol code into the app.
3. **Install the app as a priv-app** like MSD. Rejected: no privileged
   permission is needed, and KernelSU module mounting adds setup steps.

## Repository layout

```
Cargo.toml              Rust workspace
uvcd/                   privileged daemon/CLI (Rust)
app/                    Android app (Kotlin, Compose)
app/module/             Magisk/KernelSU module scripts
docs/ARCHITECTURE.md    this file
docs/PLAN.md            phased plan and test strategy
```

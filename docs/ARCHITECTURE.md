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
    ├─ encoder/        HW JPEG | libjpeg-turbo MJPEG
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

| Subcommand | Purpose |
|---|---|
| `doctor [--probe-function]` | Read-only device capability report |
| `gadget up/down` | Root-only bring-up with a test pattern, no daemon or app |
| `sepatch` | Live SELinux policy patch (`post-fs-data.sh`) |
| `daemon` | Long-running service behind the supervisor |
| `client status/enable/disable/source` | Debug CLI speaking the app protocol; `source` feeds a reversed test pattern through the frame source channel |

Process model (from MSD): `service.sh` → `app_process` runs
`standalone.AuthSupervisor` as root. The supervisor spawns
`runcon u:r:uvcamera_daemon:s0 uvcd daemon` with its stdin/stdout as a private
auth pipe. The daemon drops to `system:system`, keeping only `CAP_CHOWN`
(configfs creates new nodes as `root:root`, so it must `chown` them),
`CAP_SYS_NICE` (USB IRQ thread priority) and the `readproc` group (to see
kernel threads through `/proc`'s `hidepid`). Where a gadget HAL keeps the
gadget `root:root` (0770, attributes 0644) instead of init handing it to
system, it also keeps `CAP_DAC_OVERRIDE`. It listens on `@uvcamerad` and
asks the supervisor to authorize every peer UID: at connect, and again on
every control request.

Threads:

- **Main:** accepts connections (polling the listener so SIGTERM is noticed).
- **One per client connection:** control requests, or, after
  `AttachSource`, reading frames into the `SourceHub`.
- **Stream thread (`session.rs`), one per enabled session:** `poll()`s
  `/dev/videoN` (V4L2 events, `POLLOUT` for completed buffers) and the
  `SourceHub` eventfd (a new app frame arrived). It answers UVC control
  requests, queues frames, and reopens the node when Android rebinds the
  gadget.

The UVC request logic is a plain struct, `uvc::Protocol`: it consumes SETUP
and DATA payloads and returns replies, so it's unit-tested on the host
(including request sequences captured from Windows and Linux).

Frame policy (`stream.rs`): a fresh app frame is queued as soon as it
arrives and a buffer is free. If none arrives within 1.5 frame intervals,
the last one is repeated (for up to 1 s), then the test pattern is sent as a
"camera not ready" placeholder. YUYV frames must be exactly w×h×2 bytes and
MJPEG frames must start with a JPEG SOI marker and fit the negotiated size.

### App (Kotlin + Compose, `app/`)

- **WebcamService**: foreground service with
  `foregroundServiceType="camera"`. It must be started while the activity is
  visible, because Android 11+ denies camera access to an FGS started from the
  background. It then stays alive while webcam mode is enabled, holds the
  daemon connection, and opens the camera only between the daemon's
  `StreamStart` and `StreamStop`. The privacy indicator is therefore on only
  while the host is actually streaming.
- **camera/**: `queryCameras` reads Camera2 capabilities, from which
  `webcamFormats` builds the formats to advertise (standard sizes the camera
  delivers at ≥ 30 fps, capped by the user's maximum resolution).
  `CameraStreamer` opens the selected camera with one `ImageReader`, using
  `TEMPLATE_PREVIEW` with video stabilization off (lower latency), the AE
  target fps range pinned to the stream's rate, and continuous-video AF.
- **Encoding**, chosen per stream:

  | Path | Output | Cost (OnePlus 15, 1080p) | When |
  |---|---|---|---|
  | Camera2 `JPEG` stream | MJPEG | 0.7 ms copy per frame | The camera supports the size at the frame rate and no rotation is needed |
  | `YUV_420_888` → NV21 → `YuvImage` | MJPEG | ~38 ms per frame, two worker threads → 30 fps | Smaller sizes without hardware JPEG (720p/480p here), or rotated |

- **Orientation:** sensor images arrive unrotated. For a landscape mount (top
  of the phone to the left) the correction is `sensor − 90°` for back
  cameras and `sensor + 90°` for front cameras, since turning the phone looks
  opposite to each; on the OnePlus 15 (90°/270°) neither needs a turn. The
  user adds 0/90/180/270° and left/right, up/down mirroring on top.
  `YuvTransform` rotates, mirrors and center-crops during the YUV
  conversion (each output row is a straight walk through the source); for
  quarter turns the capture size is the smallest that covers the output
  (1080p → 2560×1920). The camera's JPEG encoder can't transform, so any
  transform uses the YUV path. Changes apply live: the encoder swaps
  transforms when the capture size still fits, otherwise the camera
  restarts on the same host stream.
- **Controls:** host control values arrive as `Control` events and map to
  Camera2: brightness → AE compensation, zoom → `CONTROL_ZOOM_RATIO`, focus
  → AF off + `LENS_FOCUS_DISTANCE`, power line → AE antibanding, white
  balance temperature → `COLOR_CORRECTION_MODE_CCT` (Android 16), else
  gains from the sensor calibration (DNG method), else the nearest AWB
  preset. Controls are advertised for all cameras together, and each camera
  clamps or skips what it lacks.
- **Live camera switch:** another camera with the same formats reopens on
  the host's current stream with the host's control values (~110 ms, a few
  repeated frames). A change of formats (resolution cap) re-enables the
  daemon, which the host sees as a replug.
- **FrameSender:** one pending frame per source connection, so a slow socket
  drops stale frames instead of adding latency.

## Session lifecycle

```
 App (WebcamService)        uvcd                         Kernel / Host
  │ user taps Enable         │                              │
  │── control: connect ─────►│ authorize UID via supervisor │
  │── Enable(FormatSpec) ───►│ UDC="", mkdir uvc.uvcamera,  │
  │                          │ write descriptors, link b.1, │
  │                          │ set bDeviceClass EF/02/01,   │
  │                          │ UDC=<controller>             │
  │                          │ open /dev/videoN (function_name match)
  │── source: connect,       │                              │
  │   AttachSource ─────────►│ SourceHub.attach             │
  │                          │◄──────── UVC_EVENT_CONNECT ──│ host enumerates
  │                          │◄─ SETUP/DATA: PROBE, COMMIT ─│ host opens camera
  │                          │── UVCIOC_SEND_RESPONSE ─────►│
  │                          │◄──────── UVC_EVENT_STREAMON ─│
  │                          │ S_FMT, S_PARM, REQBUFS, STREAMON,
  │                          │ IRQ thread → SCHED_FIFO      │
  │◄─ StreamStart(id, fmt) ──│ (placeholder until frames)   │
  │ open camera, encode      │                              │
  │── Frame(id, pts, bytes) ►│ eventfd → copy → QBUF ──────►│ isoc to host
  │   … (newest wins)        │                              │
  │                          │◄──────── UVC_EVENT_STREAMOFF │ host closes
  │◄─ StreamStop(id) ────────│ STREAMOFF, free buffers,     │
  │ close camera             │ restore IRQ thread           │
  │── Disable ──────────────►│ unlink + rmdir function,     │
  │                          │ restore device class, rebind │
```

If the app or its source connection dies while the function is linked, the
daemon keeps the gadget up and sends the placeholder. When the app
re-attaches it resumes without re-enabling (which would re-enumerate). An
explicit Disable, or a reboot, removes the function.

On devices with a USB gadget HAL, MSD pauses the HAL (SIGSTOP) while
reconfiguring. That isn't implemented yet: the OnePlus 15 configures the
gadget from init scripts and has no gadget HAL.

## Format advertisement

UVC descriptors are fixed at enumeration, so the advertised formats must be
decided **before** the UDC is bound. Changing them means a re-enumeration,
which drops ADB for about a second. The app derives the `FormatSpec` from the
selected camera's streamable sizes (≥ 30 fps), clipped to bandwidth:

| Format | Default frames | Bandwidth (USB 2.0 HS isoc, ~24 MB/s theoretical) |
|---|---|---|
| MJPEG | 1920×1080, 1280×720, 640×480 @ 30/15 fps; 60 fps up to 720p where the camera can | 1080p30 ≈ 3–9 MB/s, fine; 1080p60 doesn't fit at the minimum burst |

The app doesn't offer YUYV: uncompressed 640×480@30 is 18.4 MB/s, at the
limit, and higher sizes don't fit. uvcd still supports it for
`uvcd gadget up`.

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

Abstract Unix stream socket `@uvcamerad`, protocol version 4 (`message.rs`,
mirrored by `Message.kt`, with golden-byte tests on both sides). After a
version handshake, a connection carries tagged little-endian messages in
MSD's style. Malformed input closes the connection.

- **Control connection:** request/response. `GetStatus` → `Status` (enabled,
  host link speed, current stream, fps sent, fps received from the app,
  failed frames), `Enable(maxburst, formats, controls)`, `Disable`. The
  control capabilities (zoom range, focus, white balance, power line) set
  the units' `bmControls`; brightness is always offered.
- **Source connection:** a second connection sends `AttachSource` and then
  carries frames. The daemon sends `StreamStart(stream_id, params)`,
  `StreamStop(stream_id)` and `Control(id, value)`; on attach it replays
  the current control values and stream. The app sends `Frame(stream_id, pts_us, bytes)`,
  where `pts_us` is the sensor capture time in CLOCK_MONOTONIC (for latency
  stats). Frames for an old `stream_id` are dropped, so a format switch
  can't show stale frames. Frames are authorized once at attach, not per
  frame: they can only change what the host sees, not the gadget.

MJPEG at 1080p30 is about 10 MB/s and YUYV at 640×480@30 about 18 MB/s, easy
for a Unix socket. The daemon copies each frame into a mmap'd V4L2 buffer
(~0.2 ms). A memfd ring with `V4L2_MEMORY_USERPTR`/`DMABUF` would avoid the
copy, but measurements don't call for it.

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
- **USB mode changes:** devices without a gadget HAL
  (`vendor.usb.use_gadget_hal=0`, e.g. Qualcomm/OnePlus) reconfigure through
  init property triggers, which only remove the `f1…f9` links, so our
  uniquely named link survives; the daemon reopens the video node after the
  rebind. If a switch drops our link (a gadget HAL rebuilding the config),
  the daemon re-links once the gadget is bound again. Pausing a gadget HAL
  (`SIGSTOP`, MSD's approach) while we reconfigure is not implemented yet:
  the test phone has none. See the device notes in PLAN.md.
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
4. **Android USB accessory mode (AOA), without root.** Rejected: the
   accessory function's descriptors are fixed and vendor-specific, so hosts
   never load their UVC driver. It would need host software and a virtual
   camera on every OS.
5. **Our own UVC function module with bulk transfers and H.264**
   (`kmod/`). Built and working, but rejected: hosts support H.264 poorly
   (Windows Camera ignores it, OBS adds latency), and MJPEG 1080p30 already
   fits isochronous transfers. The H.264 path was removed; the module stays
   for bulk experiments with `uvcd gadget up`. See PLAN.md, "Experiment:
   kernel module with bulk + H.264".

## Repository layout

```
Cargo.toml              Rust workspace
uvcd/                   privileged daemon/CLI (Rust)
app/                    Android app (Kotlin, Compose)
app/module/             Magisk/KernelSU module scripts
kmod/                   experimental usb_f_uvcx kernel module (bulk)
docs/ARCHITECTURE.md    this file
docs/PLAN.md            phased plan and test strategy
```

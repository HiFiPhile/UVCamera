# UVCamera plan

The phases are ordered to retire the biggest risks first. Phase 1 proves that
a phone can stream to a host **without** the app, the camera, IPC or SELinux
work. If that fails on the target devices, nothing else matters.

## Phase 0: scaffold and toolchain ✅

- [x] Local toolchain: JDK 21, SDK platform 37, build-tools 37.0.0, NDK r30,
      Rust + Android targets, `cargo-android`
- [x] Gradle project (AGP 9.4, Kotlin 2.4, Compose), version catalog, wrapper
      (checksum pinned)
- [x] Rust workspace, `uvcd` crate built per ABI by Gradle
- [x] Module ZIP (`./gradlew zipDebug`): `uvcd.<abi>`, APK, scripts
- [x] `uvcd doctor [--probe-function]`: device capability report
- [x] App screen: per-camera sizes streamable at ≥ 30 fps as JPEG / YUV
- [x] Architecture and plan docs

**Exit:** `./gradlew build zipDebug` is green. ✅

**Action for you:** flash the debug ZIP on each target phone, reboot, and
collect `/data/local/tmp/uvcamera/doctor.log`, plus a screenshot of the app's
camera screen. These two inputs decide Phase 1 scope and the encoder strategy.

## Phase 1: gadget bring-up with a test pattern (no app)

Goal: the host shows a moving test pattern from `uvcd gadget up`, run as root
through `adb shell su`. This validates the kernel, UDC bandwidth, gadget
HAL/init interplay and host compatibility.

- [x] `gadget.rs`: configfs ops (UDC bind/unbind, config link named
      `uvcamera`, IAD device class `EF/02/01`) and the UVC function builder:
      control header + class links, MJPEG/YUYV formats and frames, streaming
      header + fs/hs/ss class links, `streaming_maxpacket/maxburst`,
      `function_name`. Kernel-assigned `bFormatIndex`/`bFrameIndex` and
      entity IDs are read back. Teardown handles partial state.
- [x] `v4l2.rs`: ioctl bindings (events, `UVCIOC_SEND_RESPONSE`, S_FMT,
      MMAP buffers, STREAMON/OFF). Struct sizes are asserted at compile time
      for 64-bit and armv7, and ioctl numbers are checked against the
      kernel headers.
- [x] `uvc/`: PROBE/COMMIT negotiation (GET_CUR/MIN/MAX/DEF/RES/LEN/INFO,
      SET_CUR with data stage), request-error-code control, and the controls
      the kernel's default descriptors advertise (CT AE mode, PU
      brightness). Unit tested.
- [x] `pattern.rs`: color bars, a sweeping box and a 00–59 counter. YUYV is
      rendered per frame; MJPEG is encoded with `jpeg-encoder` and cached
      for a 60-frame loop.
- [x] `stream.rs`: poll loop over V4L2 events and buffer completions, with
      frames paced to the committed interval. Reopens the node if a USB mode
      switch rebinds the gadget.
- [x] `gadget up` / `gadget down`, with a state file so `down` restores
      everything after a crash.
- [x] On-device dry run (OnePlus 15, no host attached): build, bind, node
      discovery (`/dev/video2`, `g_uvc`), subscribe and teardown all work;
      the original config is restored.
- [x] Host test: Windows 11 Camera app (2026-10-12). It picked MJPEG
      1920×1080 and held 30.0 fps with 0 late frames over 24 s and 137 s
      sessions. MTP kept working alongside. The link was High Speed (USB 2
      cable/port). On plug-in Android rebound the gadget; the video node
      disappeared and uvcd reopened it automatically, so our link survives
      the vendor init's USB handling. Windows also probes CT exposure time
      (0x04) and iris (0x09) with GET_INFO and reads the error code after our
      stalls.
- [x] Host test: SuperSpeed (USB 3 cable/port), MJPEG 1080p30 and 480p30,
      0 late frames.
- [x] Host test: YUYV 640×480@30 on Windows over SuperSpeed. Correct colors,
      but laggy at first; fixed (see "Isochronous reliability" below). Now
      30.0 fps with 0 failed frames.
- [x] MJPEG 60 fps (`--mjpeg-fps 60 --mjpeg-fps 30 …`) over SuperSpeed on
      Windows: 1080p60 and 720p60 sent at 60.0 fps; 1 failed frame in
      ~1,120 at 1080p60 (~0.1%). The Camera app opened at 1080p30 and used
      60 fps once it was selected. The pure-Rust test pattern encoder needs
      up to 42 ms per 1080p frame before caching, so real 1080p60 needs a
      hardware or NEON JPEG encoder (Phase 3).
- [x] Live stats every 5 s: frames sent, frames the kernel flagged as not
      fully transferred, starvation, max completion gap, produce time.
- [x] Host test: Linux `uvcvideo` (2026-10-13), SuperSpeed. `v4l2-ctl
      --list-formats-ext` lists everything; `ffplay` shows MJPEG 1080p60 at
      59.9 fps (3 of 1383 frames failed, ~0.2%), YUYV 640×480@30 at 30.0 fps
      (6 of 675 failed, ~0.9%, shown by ffmpeg as "dequeued v4l2 buffer
      contains corrupted data"), and YUYV 1280×720@10. Linux delivers failed
      frames flagged as errors, whereas Windows silently drops them.
- [ ] Host test: macOS
- [x] Host request sequences captured as unit tests:
      `windows_11_camera_app_sequence`, `linux_uvcvideo_sequence` (Linux
      probes every format/frame with SET_CUR/GET_MIN/GET_MAX/SET_CUR/GET_CUR).
      macOS still to capture.

Default formats: MJPEG 1920×1080, 1280×720 and 640×480 at 30/15 fps; YUYV
640×480 at 30/15 and 1280×720 at 10. The isochronous endpoint defaults to
`maxpacket=3072`, `maxburst=0` (24.5 MB/s at both HS and SS).

### Isochronous reliability (GKI 6.12, OnePlus 15)

YUYV 640×480@30 looked laggy without artifacts. Windows was discarding
incomplete frames, which the kernel returns with `V4L2_BUF_FLAG_ERROR` after
a missed isochronous slot (`-EXDEV`). Measured failed frames:

| Setup | Failed frames |
|---|---|
| `maxburst 6` (21.5 KB per 125 µs request) | ~22% |
| `maxburst 0` (3 KB requests) | ~6% |
| `maxburst 0` + dwc3 IRQ thread at SCHED_FIFO 50 | 0% |

Causes:
- **Request pool too shallow.** Pre-6.13 f_uvc uses
  `req_size = maxpacket × burst × mult` and allocates
  `clamp(imagesize / 2 / req_size, 4, 64)` requests, about half of them in
  flight. With large bursts only ~1 ms is buffered. Small requests hit the
  64 cap, about 4 ms. 6.13+ instead spreads frames over the interval set by
  `VIDIOC_S_PARM`, which uvcd sends; 6.12 returns ENOTTY.
- **IRQ thread priority.** The Qualcomm dwc3 driver runs its threaded IRQ
  handler as SCHED_NORMAL ("Set IRQ thread … to SCHED_NORMAL prio" in
  dmesg), so CPU load delays request completions past the buffered time.
  uvcd now raises `irq/<n>-<udc driver>` to SCHED_FIFO 50 at stream start
  and restores it at stop (`--no-irq-boost` disables this). The thread is
  recreated, and reset by the vendor driver, on every gadget bind.

Residual failures with the boost: 0% on the Windows PC, ~0.2% (MJPEG
1080p60) and ~0.9% (YUYV 640×480@30) on the Linux PC. Not CPU idle exit,
since this phone's idle states wake in ≤ 302 µs. Candidates still to try:
(1) moving the dwc3 IRQ off the busy CPU0/1 (`/proc/irq/<n>/smp_affinity`);
(2) the host not polling in time, which dwc3 also reports as a missed
slot, would explain the Windows/Linux difference; (3) a deeper request
pool via `streaming_interval` 2 + `maxburst` 1 (same 24.5 MB/s, 6.25 ms
buffered for 640×480 YUYV).

Rule of thumb: use the smallest `maxburst` that covers the largest advertised
uncompressed frame rate, because bigger bursts shrink the timing margin on
pre-6.13 kernels.

**Exit:** test pattern plays in `ffplay /dev/videoX` on Linux, the Windows
Camera app, and macOS QuickTime/FaceTime. ADB stays usable throughout. Repeated
up/down cycles leave configfs clean.

**Dev loop without a phone (recommended):** a Linux VM with `libcomposite`,
`usb_f_uvc` and `dummy_hcd`. `uvcd` already builds for `target_os = "linux"`,
and the gadget and the host driver run in the same VM, so
`gadget up --gadget /sys/kernel/config/usb_gadget/test` is visible to
`v4l2-ctl --list-devices`. The current dev container is an LXC guest and
cannot load kernel modules, so this needs a full VM.

## Phase 2: daemon, SELinux, auth and app connection

- [x] `uvcd sepatch`: the `uvcamera_daemon` domain via the `sepatch` crate
      (`sepatch.rs`); `--permissive` for collecting denials, `--allow-adb`.
      `post-fs-data.sh` loads it at boot.
- [x] `standalone/AuthSupervisor.java`, ported from MSD-KSU: a root
      `app_process` that starts the daemon via `runcon` and answers per-UID
      authorization over private pipes (package name + signing certificate
      pin). Gradle `clientCertificate<Variant>` writes `client-cert.sha256`.
- [x] `uvcd daemon` (`daemon.rs`): root → `system`, keeping CAP_CHOWN,
      CAP_SYS_NICE and the `readproc` group; SELinux self-check; listener on
      `@uvcamerad`; every request re-authorized. `session.rs` shares the
      gadget lifecycle with `gadget up`.
- [x] Protocol v1 (`message.rs` / `Message.kt`): GetStatus, Enable (full
      format list + maxburst), Disable; Status includes host speed, the
      current stream and live fps/failed counts. Golden-byte tests on both
      sides. `uvcd client status|enable|disable`.
- [x] App: `WebcamCard` shows daemon reachability, host link and stream
      stats, with Enable/Disable (test pattern for now).
- [x] Module boot path on a real install (2026-10-13, KernelSU with the
      Hybrid Mount metamodule; `ksud module install`, reboot): post-fs-data
      `sepatch` exit 0; service.sh installed the app (no copy present),
      wrote the doctor report and started the supervisor; the daemon ran as
      `system` in `uvcamera_daemon` with groups 3009 and CHOWN + SYS_NICE;
      the app enabled and disabled the webcam; 0 denials since boot.
      Notes: KernelSU blocks installs while a metamodule has pending
      changes (reboot first); wireless debugging turns off on reboot.

Verified on the OnePlus 15 (2026-10-13), SELinux enforcing, with the policy
and supervisor started by hand: the daemon runs as `system` in
`u:r:uvcamera_daemon:s0` (`CapEff 0x800001`, groups `3009`); the app enables
the gadget and the Linux host streams the test pattern (MJPEG 1080p30, 30.0
fps, ~0.3% failed frames); the supervisor ALLOWs the app's UID and DENYs
another package's UID and the same app ID in another user; a uid 2000 shell
client is rejected at connect; 0 `uvcamera_daemon` denials (on this phone
denials go to logcat via auditd, not dmesg). MSD's supervisor and daemon ran
alongside the whole time.

Pitfalls found:
- Android mounts /proc with `hidepid=invisible,gid=readproc`, so after the
  privilege drop the IRQ boost silently found no kernel threads and frame
  loss went back up (~1.5%) until the daemon kept group 3009.
- The SELinux self-check needs `connect` on the daemon's own socket, so the
  `connectto` denial (the actual check) is reached silently.
- On this phone `adb shell` is root (KernelSU), i.e. a root client; use
  `su 2000` to test unprivileged rejection.

## Phase 3: camera to host

- [x] Protocol v2 source channel (`AttachSource`; StreamStart/StreamStop
      with per-stream ids; Frame), `source.rs` hub (newest wins, stale ids
      dropped, write timeout), placeholder pattern when there is no fresh
      app frame for 1 s. `uvcd client source` feeds a reversed pattern for
      testing without the app.
- [x] `WebcamService`: foreground service of type camera, started from the
      UI (camera permission; notification with a Turn off action). Enables
      the daemon with formats derived from the back camera's capabilities
      (`webcamFormats`), attaches as the source, opens the camera only
      between StreamStart and StreamStop, reconnects if the daemon restarts
      without re-enabling (which would re-enumerate).
- [x] `CameraStreamer` (Camera2, one ImageReader output, TEMPLATE_RECORD,
      AE target range pinned to the fps, continuous-video AF) with
      per-stream encoders: hardware JPEG where the camera supports the size
      at the rate, else YUV → `YuvImage` JPEG; YUV → YUYV for uncompressed.
      `FrameSender` keeps one pending frame (newest wins).
- [ ] Camera choice (front/back) and resolution caps in the UI; persist.
- [ ] libjpeg-turbo (NEON) if `YuvImage` can't keep up at some size; not
      needed so far.

Verified on the OnePlus 15 with the Linux host (2026-10-13): MJPEG
1920×1080@30 (hardware JPEG), MJPEG 1280×720@30 (YuvImage), YUYV
640×480@30 all show live video. Daemon stats: 30.0 fps sent, 30.0 fps from
the camera after ~0.5 s of placeholder while the camera opens, 0 failed
frames; ffplay shows no corrupted-frame warnings.

**Exit:** 1080p30 MJPEG to Windows, macOS and Linux for 1 hour without leaks.
Latency is measured with a stopwatch test (target < 150 ms glass-to-glass).
Screen off works, or the limitation is documented.
- [x] Linux, Windows (Camera app)
- [ ] macOS
- [ ] 1 hour soak (memory of app and daemon stable)
- [x] Latency (Windows Camera, 1080p30 MJPEG, stopwatch): 250 ms at first,
      **210 ms** after queueing app frames as they arrive (they used to wait
      for the next 33 ms tick) and capturing with TEMPLATE_PREVIEW and video
      stabilization off. The daemon now logs capture-to-USB-queue latency
      from the sensor timestamp: **104 ms** (131 ms after AE lengthened the
      exposure, presumably; the timestamp is the start of exposure), of which
      0.1 ms is waiting in the daemon. The remaining ~80–105 ms is USB
      transfer plus Windows' decode, buffering and display. The camera
      pipeline dominates, so the 150 ms target isn't reachable from our
      side; 210 ms is typical for a USB webcam with Windows Camera.
      Follow-ups in Phase 4: compare the YUV path at 1080p with hardware
      JPEG; log exposure time next to latency.
- [x] Screen off (Android 16, OnePlus 15): video keeps going. The camera
      dipped to 26.6 fps in the 5 s window where the screen turned off, then
      held 29.4–30 fps with no placeholder or failed frames. The foreground
      service started from the UI keeps camera access.

## Phase 4: quality and robustness

- UVC controls mapped to Camera2: zoom (`CONTROL_ZOOM_RATIO`), brightness
  (AE compensation), auto/manual focus, white balance auto, power-line
  frequency (AE antibanding).
- YUYV format. Hot-switch the camera (front/back) without re-enumeration.
- Re-apply after USB mode changes (inotify on `configs/b.1`, UDC state).
- memfd ring + `USERPTR`/`DMABUF` if profiling shows copy overhead.
- In-app preview, per-host quirks, MSD coexistence test.
- Release signing, `update.json` for module updates, GitHub Actions CI
  (adapt MSD's `ci.yml`).

## Phase 5: stretch

- H.264 via UVC 1.5 frame-based format, when the kernel exposes
  `streaming/framebased`. MediaCodec with a Surface input from Camera2.
- Microphone as UAC2 (`f_uac2`) so the phone is a webcam + mic.
- Still image capture (method 2).

## Test strategy

| Layer | How |
|---|---|
| `uvc/` protocol, descriptor math, message codec | `cargo test` on the host. Golden request sequences per host OS. |
| `gadget/` configfs builder | Linux VM with dummy_hcd (see Phase 1). A trait-mocked fs for unit tests. |
| App logic (capabilities, format selection, codec) | JVM unit tests (`./gradlew test`) |
| End-to-end | Device matrix × host matrix below, manual checklist per release |

Device matrix (to fill from `doctor.log`):

| Device | Android | Kernel | F_UVC | UDC / max speed | Root | Status |
|---|---|---|---|---|---|---|
| OnePlus 15 (CPH2747, Qualcomm) | 16 (API 36) | 6.12.58 GKI android16 | `=y` | a600000.dwc3 / super-speed(-plus) | KernelSU | doctor ✅, Phase 1 ✅ on Windows (MJPEG 1080p30) |

### Device notes: OnePlus 15 (CPH2747), 2026-10-12

- **No gadget HAL.** `vendor.usb.use_gadget_hal=0`, and the gadget is driven
  by `/vendor/etc/init/hw/init.qcom.usb.rc` property triggers on
  `sys.usb.config`. Every mode switch runs `rm configs/b.1/f1…f9`, then
  relinks and rewrites `idVendor`/`idProduct`/`UDC`. A link with a different
  name (e.g. `uvcamera`) is not removed, so the function should survive mode
  switches. Verify this in Phase 1. There is no HAL process to `SIGSTOP`; the
  USB HAL is `android.hardware.usb-service.qti` (`vendor_hal_usb_qti`), which
  only handles port roles.
- **Vendor `uvc.0` and `uac2.0` already exist**, pre-built at boot by
  `on property:vendor.usb.uvc.function.init=1`. They have MJPEG and YUYV
  360p/720p/1080p frames, but only 1080p MJPEG is at 30 fps (the others are
  2 fps), and a dangling `streaming/h264/h` symlink. It's only used by the
  `diag,uvc…` debug compositions (VID 0x05C6). We leave it alone and create
  our own function.
- **`com.android.DeviceAsWebcam` is installed** (`/system/priv-app`), but
  `ro.usb.uvc.enabled` is unset, so the stock webcam mode is hidden. The
  vendor init has no plain `uvc`/`uvc,adb` composition, so enabling the prop
  alone would likely not work.
- `streaming_maxpacket=3072`, `streaming_maxburst=6` are the vendor defaults
  for SuperSpeed.
- `/dev/video*` nodes are `system:camera 0660`, `u:object_r:video_device:s0`.
  The gadget node will appear as a new `/dev/videoN` with a `function_name`
  sysfs attribute.
- Camera2: 2 logical cameras (0 back, 1 front), both LEVEL_3. JPEG is
  reported at ≥ 30 fps for all listed sizes, but the **smallest JPEG size is
  1920×1080**. MJPEG at 720p/480p therefore needs the YUV → software JPEG
  path (or a downscale). YUV is available at 30 fps from 4096×3072 down to
  176×144. Whether the 30 fps JPEG repeating stream holds up in practice is
  still to be measured.

Host matrix: Windows 11 (Camera app, Teams/Zoom), macOS (FaceTime/QuickTime,
Chrome), Linux (`ffplay`, `cheese`, Chrome).

## Open questions and risks

| Risk | Impact | Mitigation |
|---|---|---|
| Target phones lack `F_UVC` | Blocker | `doctor` first. Only GKI ≥ android14 is assumed. |
| Gadget HAL resets the config on replug/mode change | Webcam vanishes | HAL stop during config (proven by MSD). Watchdog re-apply in Phase 4. |
| Camera access from FGS with screen off / app in background | Stream stops | FGS started from foreground keeps while-in-use access. Verify early in Phase 3. |
| Camera2 JPEG repeating stream slow or poor on some HALs | Low fps | libjpeg-turbo fallback (strategy B) |
| Isochronous bandwidth limited by the UDC | Low res/fps | MJPEG by default. Tune `maxpacket`/`maxburst`. Report the negotiated speed. |
| Older f_uvc (GKI 5.10) bugs or missing ioctls | Glitches | Own format table. Avoid ENUM_FMT/S_PARM reliance. Test on 5.10. |
| Windows composite enumeration | Not recognized | IAD device class. Test with and without ADB enabled. |

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

Goal: the host shows a moving test pattern from `uvcd gadget up --test-pattern`
run as root through `adb shell su`. This validates the kernel, UDC
bandwidth, gadget HAL interplay and host compatibility.

1. Port MSD's `usb.rs` into `uvcd/src/gadget/` (configfs helpers, HAL
   stopper, UDC rebind).
2. `gadget/uvc.rs`: build `functions/uvc.uvcamera` from a `FormatSpec`
   (control header + class links, streaming header, uncompressed/mjpeg
   formats and frames, color matching, `streaming_maxpacket`,
   `function_name`). Teardown in reverse order.
3. `v4l2/`: minimal ioctl bindings (`rustix::ioctl`) and the uapi structs from
   `linux/videodev2.h` and `linux/usb/g_uvc.h`.
4. `uvc/`: PROBE/COMMIT state machine (GET_CUR/MIN/MAX/DEF/LEN/INFO,
   SET_CUR), request-error-code control, stub camera/processing unit
   controls. **Host unit tests** with captured request sequences from
   Linux/Windows/macOS.
5. Test source: YUYV moving color bars (no encoder needed). Optionally add
   MJPEG via the pure-Rust `jpeg-encoder` crate for the test pattern only.
6. `gadget up` / `gadget down` CLI; restore the device class and links on
   exit/SIGTERM.

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

1. `uvcd sepatch`: the `uvcamera_daemon` domain (rules in ARCHITECTURE.md),
   using the `sepatch` crate. Add `post-fs-data.sh`.
2. Port `AuthenticatedDaemon.java` → `standalone/AuthSupervisor.java`.
   Gradle task for `client-cert.sha256` (from MSD).
3. `uvcd daemon`: privilege drop, `@uvcamerad` listener, protocol v1
   (`ipc/message.rs`), `uvcd client` debug CLI.
4. App `daemon/Client.kt` + `Message.kt`. UI shows daemon status and
   gadget state, with Enable/Disable buttons driving the test pattern
   through the daemon.

**Exit:** with SELinux enforcing, the app enables the gadget and the host
sees the test pattern. A differently signed app and `adb shell` (uid 2000) are
rejected. `ps -Z` shows `uvcamera_daemon`. No new denials in `dmesg | grep
avc`.

## Phase 3: camera to host (MJPEG)

1. `WebcamService` FGS (`camera` type, notification with Stop action).
   Start it from the UI only.
2. `FormatSpec` from camera capabilities, with user choice of camera and
   resolution caps. Persist it in preferences.
3. Encoder strategy A: Camera2 JPEG repeating request where supported.
   Strategy B: `YUV_420_888` → libjpeg-turbo via JNI (CMake in
   `app/src/main/cpp`, libjpeg-turbo as a pinned submodule).
4. Frames over the socket. Newest-wins backpressure. PTS and frame-drop
   counters in Status.
5. Placeholder frame when the camera is unavailable or the app disconnects.

**Exit:** 1080p30 MJPEG to Windows, macOS and Linux for 1 hour without leaks.
Latency is measured with a stopwatch test (target < 150 ms glass-to-glass).
Screen off works, or the limitation is documented.

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
| OnePlus 15 (CPH2747, Qualcomm) | 16 (API 36) | 6.12.58 GKI android16 | `=y` | a600000.dwc3 / super-speed(-plus) | KernelSU | doctor ✅, Phase 1 next |

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

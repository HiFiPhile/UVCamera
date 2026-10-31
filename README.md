# UVCamera

Use a rooted Android phone as a USB webcam. UVCamera uses the kernel's UVC
gadget function (`CONFIG_USB_CONFIGFS_F_UVC`), so the host sees a standard
USB Video Class camera and needs no drivers or apps. ADB and MTP keep working.

The design follows [MSD](https://github.com/HiFiPhile/MSD): a Magisk/KernelSU
module runs a small privileged Rust daemon (`uvcd`) that configures the USB
gadget. The app captures from the camera with Camera2 and streams frames to the
daemon. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
[docs/PLAN.md](docs/PLAN.md).

**Status:** early development (Phase 4). Turn the webcam on in the app and the
host sees any of the phone's cameras as MJPEG, up to 1080p60 (1080p30 on
phones whose cameras can't do 60). The camera is on only while an app on the
host is using it. `uvcd gadget up` still works for bring-up with a test
pattern and no app.

## Requirements

- Android 11+, rooted with Magisk or KernelSU (SELinux stays enforcing)
- configfs USB gadget: `ls /config/usb_gadget/g1`
- UVC gadget support in the kernel:
  `zcat /proc/config.gz | grep CONFIG_USB_CONFIGFS_F_UVC` should be `=y`

## Using it

1. Install the module ZIP with Magisk or KernelSU and reboot. It installs the
   UVCamera app.
2. Open UVCamera, pick the camera, and tap **Turn on** (allow the camera
   permission). A "USB webcam is on" notification appears.
3. Plug the phone into the computer and choose "UVCamera" in any camera app.
   The phone's camera turns on only while the computer is using it.

The camera and the orientation apply live, without reconnecting the webcam.
Rotate by 0°, 90°, 180° or 270° to match how the phone is mounted, and mirror
left/right or up/down. The host always sees the same formats: a camera that
can't fill a size gets black bars, and one limited to 30 fps has its frames
repeated in a 60 fps stream. The host's brightness, zoom, focus, white
balance and power line frequency controls drive the camera, where it
supports them. The **Device** page shows what the phone and each camera can
do. Turn the webcam off from the app or the notification.

## Checking a device

Install the module ZIP and reboot. Then read the report:

```sh
adb shell su -c cat /data/local/tmp/uvcamera/doctor.log
```

Or run it by hand:

```sh
adb push target/aarch64-linux-android/release/uvcd /data/local/tmp/
adb shell su -c /data/local/tmp/uvcd doctor --probe-function
```

`--probe-function` creates and immediately removes an unlinked UVC function in
configfs. It never changes the active USB configuration.

## Streaming a test pattern

Over wireless ADB (USB ADB drops while the gadget is rebound), with the phone
plugged into the host:

```sh
adb push target/aarch64-linux-android/release/uvcd /data/local/tmp/
adb shell su -c /data/local/tmp/uvcd gadget up        # runs until stopped
adb shell su -c /data/local/tmp/uvcd gadget down      # from another shell
```

The host sees a "UVCamera" webcam offering MJPEG and YUYV. `gadget down` (or
Ctrl-C) removes the function and restores the original USB configuration.
Options: `--format mjpeg|yuyv` (repeatable), `--mjpeg-fps N` (repeatable,
e.g. `--mjpeg-fps 60 --mjpeg-fps 30`), `--maxburst N` for SuperSpeed,
`--log-level debug` to log every control request.

## Daemon

The module patches SELinux at boot (`uvcd sepatch`), then starts a root
supervisor that launches `uvcd daemon` as the `system` user in the
`uvcamera_daemon` domain. Only the UVCamera app signed with the certificate
pinned in the module (`client-cert.sha256`) may use it. From a root shell:

```sh
uvcd client status
uvcd client enable [--format mjpeg|yuyv] [--mjpeg-fps N] [--maxburst N]
uvcd client disable
```

Daemon logs: `adb logcat -s uvcd`.

## Building

Toolchain: JDK 21, Android SDK (platform 37, build-tools 37.0.0, NDK
30.0.16248370, CMake 3.31.6), Rust with the Android targets, and
[`cargo-android`](https://github.com/chenxiaolong/cargo-android):

```sh
rustup target add aarch64-linux-android thumbv7neon-linux-androideabi x86_64-linux-android
cargo install --git https://github.com/chenxiaolong/cargo-android --tag v0.1.6
```

Point `local.properties` (`sdk.dir=…`) or `ANDROID_HOME` at the SDK, put `cargo`
on `PATH`, then:

```sh
./gradlew build zipDebug -x assembleRelease   # module: app/build/distributions/debug/
cargo test                                    # uvcd host unit tests
```

Signed releases, the module update channel and CI are described in
[docs/RELEASE.md](docs/RELEASE.md).

## License

GPL-3.0-only. Portions are derived from MSD by Andrew Gunnerson.

The app bundles [libjpeg-turbo](https://libjpeg-turbo.org/) (IJG,
BSD-3-Clause and zlib licenses), downloaded at build time. This software is
based in part on the work of the Independent JPEG Group.

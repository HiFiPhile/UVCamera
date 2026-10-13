# UVCamera

Use a rooted Android phone as a USB webcam. UVCamera uses the kernel's UVC
gadget function (`CONFIG_USB_CONFIGFS_F_UVC`), so the host sees a standard
USB Video Class camera and needs no drivers or apps. ADB and MTP keep working.

The design follows [MSD](https://github.com/HiFiPhile/MSD): a Magisk/KernelSU
module runs a small privileged Rust daemon (`uvcd`) that configures the USB
gadget. The app captures from the camera with Camera2 and streams frames to the
daemon. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
[docs/PLAN.md](docs/PLAN.md).

**Status:** early development (Phase 2). The module runs `uvcd` as a daemon
in its own SELinux domain; the app enables the webcam and the host sees a test
pattern. The camera path comes in Phase 3. `uvcd gadget up` still works for
bring-up without the app.

## Requirements

- Android 11+, rooted with Magisk or KernelSU (SELinux stays enforcing)
- configfs USB gadget: `ls /config/usb_gadget/g1`
- UVC gadget support in the kernel:
  `zcat /proc/config.gz | grep CONFIG_USB_CONFIGFS_F_UVC` should be `=y`

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
30.0.16248370), Rust with the Android targets, and
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

## License

GPL-3.0-only. Portions are derived from MSD by Andrew Gunnerson.

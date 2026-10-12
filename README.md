# UVCamera

Use a rooted Android phone as a USB webcam. UVCamera uses the kernel's UVC
gadget function (`CONFIG_USB_CONFIGFS_F_UVC`), so the host sees a standard
USB Video Class camera and needs no drivers or apps. ADB and MTP keep working.

The design follows [MSD](https://github.com/HiFiPhile/MSD): a Magisk/KernelSU
module runs a small privileged Rust daemon (`uvcd`) that configures the USB
gadget. The app captures from the camera with Camera2 and streams frames to the
daemon. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
[docs/PLAN.md](docs/PLAN.md).

**Status:** early development (Phase 0). The module currently installs the app
and writes a device capability report. It does not stream yet.

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

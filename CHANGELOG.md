# Changelog

### Unreleased

First release.

* Turns a rooted phone (KernelSU or Magisk) with a UVC gadget capable
  kernel into a USB webcam: MJPEG up to 1080p60, alongside ADB and MTP.
* Any of the phone's cameras, orientation (0/90/180/270°) and mirroring,
  all changeable while streaming without reconnecting. Cameras that can't
  fill a size get black bars; 30 fps cameras have frames repeated.
* Host controls: brightness, zoom, focus, white balance and power line
  frequency.
* Keeps working with the screen off and across USB mode changes.
* Device page: system details and each camera's sensor, lens, field of
  view and the webcam modes it supports.

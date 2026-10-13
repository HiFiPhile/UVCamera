// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.webcam

import com.hifiphile.uvcamera.camera.CameraInfo
import com.hifiphile.uvcamera.camera.Resolution
import com.hifiphile.uvcamera.camera.StreamFormat
import com.hifiphile.uvcamera.daemon.FormatKind
import com.hifiphile.uvcamera.daemon.FormatSpec
import com.hifiphile.uvcamera.daemon.FrameSpec
import com.hifiphile.uvcamera.daemon.fpsToInterval

private val MJPEG_SIZES = listOf(Resolution(1920, 1080), Resolution(1280, 720), Resolution(640, 480))
private val MJPEG_FPS = listOf(30, 15)

/** YUYV is limited by USB 2.0 isochronous bandwidth (~24 MB/s). */
private val YUYV_FRAMES = listOf(Resolution(640, 480) to listOf(30, 15), Resolution(1280, 720) to listOf(10))

private fun frame(size: Resolution, fps: List<Int>) =
    FrameSpec(size.width, size.height, fps.map(::fpsToInterval).sorted())

/**
 * Formats to advertise for [camera]: the standard webcam sizes that it can
 * deliver at 30 fps, as hardware JPEG or as YUV for software encoding. Falls
 * back to the daemon's defaults if the camera reports nothing usable.
 */
fun webcamFormats(camera: CameraInfo): List<FormatSpec> {
    val yuv = camera.streamableSizes[StreamFormat.YUV].orEmpty().toSet()
    val jpeg = camera.streamableSizes[StreamFormat.JPEG].orEmpty().toSet()

    val mjpeg = MJPEG_SIZES.filter { it in yuv || it in jpeg }.map { frame(it, MJPEG_FPS) }
    val yuyv = YUYV_FRAMES.filter { (size, _) -> size in yuv }.map { (size, fps) -> frame(size, fps) }

    val formats = buildList {
        if (mjpeg.isNotEmpty()) add(FormatSpec(FormatKind.MJPEG, mjpeg))
        if (yuyv.isNotEmpty()) add(FormatSpec(FormatKind.YUYV, yuyv))
    }
    return formats.ifEmpty { FormatSpec.defaults() }
}

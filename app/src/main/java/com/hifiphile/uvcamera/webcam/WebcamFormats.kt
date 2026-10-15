// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.webcam

import com.hifiphile.uvcamera.camera.CameraInfo
import com.hifiphile.uvcamera.camera.Resolution
import com.hifiphile.uvcamera.camera.StreamFormat
import android.hardware.camera2.CameraMetadata
import com.hifiphile.uvcamera.daemon.ControlCaps
import com.hifiphile.uvcamera.daemon.FormatKind
import com.hifiphile.uvcamera.daemon.FormatSpec
import com.hifiphile.uvcamera.daemon.FrameSpec
import com.hifiphile.uvcamera.daemon.fpsToInterval

private val MJPEG_SIZES = listOf(Resolution(1920, 1080), Resolution(1280, 720), Resolution(640, 480))
private val MJPEG_FPS = listOf(30, 15)
private val MJPEG_FAST_FPS = listOf(60, 30, 15)

/** YUYV is limited by USB 2.0 isochronous bandwidth (~24 MB/s). */
private val YUYV_FRAMES = listOf(Resolution(640, 480) to listOf(30, 15), Resolution(1280, 720) to listOf(10))

private fun frame(size: Resolution, fps: List<Int>) =
    FrameSpec(size.width, size.height, fps.map(::fpsToInterval).sorted())

/** Whether [camera] can stream any MJPEG size at 60 fps. */
fun supports60Fps(camera: CameraInfo): Boolean {
    val fast = camera.fastSizes.values.flatten().toSet()
    return MJPEG_SIZES.any { it in fast }
}

/**
 * Formats to advertise for [camera]: the standard webcam sizes up to
 * [maxHeight] that it can deliver at 30 fps, as hardware JPEG or as YUV for
 * software encoding. With [fps60], MJPEG sizes the camera delivers at 60 fps
 * offer 60 fps too, as the default. Falls back to the daemon's defaults if
 * the camera reports nothing usable.
 */
fun webcamFormats(camera: CameraInfo, maxHeight: Int = Int.MAX_VALUE, fps60: Boolean = false): List<FormatSpec> {
    val yuv = camera.streamableSizes[StreamFormat.YUV].orEmpty().toSet()
    val jpeg = camera.streamableSizes[StreamFormat.JPEG].orEmpty().toSet()
    val fast = if (fps60) camera.fastSizes.values.flatten().toSet() else emptySet()

    val mjpeg = MJPEG_SIZES
        .filter { it.height <= maxHeight && (it in yuv || it in jpeg) }
        .map { frame(it, if (it in fast) MJPEG_FAST_FPS else MJPEG_FPS) }
    val yuyv = YUYV_FRAMES
        .filter { (size, _) -> size.height <= maxHeight && size in yuv }
        .map { (size, fps) -> frame(size, fps) }

    val formats = buildList {
        if (mjpeg.isNotEmpty()) add(FormatSpec(FormatKind.MJPEG, mjpeg))
        if (yuyv.isNotEmpty()) add(FormatSpec(FormatKind.YUYV, yuyv))
    }
    return formats.ifEmpty { FormatSpec.defaults() }
}

/**
 * Controls to advertise for [camera]: zoom over its zoom ratio range, manual
 * focus if it has a focus motor and can turn AF off, white balance if it can
 * set colour gains from its calibration or has AWB presets to approximate a
 * temperature, and power line frequency if it offers 50/60 Hz antibanding.
 */
fun webcamControls(camera: CameraInfo): ControlCaps {
    val zoom = camera.zoomRatioRange
        ?.takeIf { (lo, hi) -> hi > lo }
        ?.let { (lo, hi) -> Math.round(lo * 100) to minOf(Math.round(hi * 100), 0xffff) }
    val focus = camera.minFocusDistance > 0f &&
        CameraMetadata.CONTROL_AF_MODE_OFF in camera.afModes &&
        CameraMetadata.CONTROL_AF_MODE_CONTINUOUS_VIDEO in camera.afModes
    val presets = camera.awbModes.count { it in AWB_PRESET_KELVIN }
    val whiteBalance = CameraMetadata.CONTROL_AWB_MODE_AUTO in camera.awbModes &&
        (camera.manualWhiteBalance || presets >= 2)
    val powerLine = CameraMetadata.CONTROL_AE_ANTIBANDING_MODE_50HZ in camera.antibandingModes ||
        CameraMetadata.CONTROL_AE_ANTIBANDING_MODE_60HZ in camera.antibandingModes
    return ControlCaps(zoom, focus, whiteBalance, powerLine)
}

/** Nominal colour temperature of each AWB preset. */
val AWB_PRESET_KELVIN = mapOf(
    CameraMetadata.CONTROL_AWB_MODE_INCANDESCENT to 2700,
    CameraMetadata.CONTROL_AWB_MODE_WARM_FLUORESCENT to 3000,
    CameraMetadata.CONTROL_AWB_MODE_FLUORESCENT to 4000,
    CameraMetadata.CONTROL_AWB_MODE_DAYLIGHT to 5500,
    CameraMetadata.CONTROL_AWB_MODE_CLOUDY_DAYLIGHT to 6500,
    CameraMetadata.CONTROL_AWB_MODE_SHADE to 7500,
)

/**
 * Controls to advertise when the user may switch between [cameras] while
 * the host is connected: everything any of them supports, so switching
 * needs no re-enumeration. Each camera clamps or ignores what it lacks.
 */
fun webcamControls(cameras: List<CameraInfo>): ControlCaps =
    cameras.map(::webcamControls).fold(ControlCaps()) { a, b ->
        ControlCaps(
            zoom = listOfNotNull(a.zoom, b.zoom)
                .reduceOrNull { x, y -> minOf(x.first, y.first) to maxOf(x.second, y.second) },
            focus = a.focus || b.focus,
            whiteBalance = a.whiteBalance || b.whiteBalance,
            powerLine = a.powerLine || b.powerLine,
        )
    }


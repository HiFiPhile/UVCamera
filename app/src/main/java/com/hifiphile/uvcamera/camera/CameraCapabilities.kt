// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import android.graphics.ImageFormat
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CameraMetadata

data class Resolution(val width: Int, val height: Int) {
    val area: Long get() = width.toLong() * height

    override fun toString() = "${width}×$height"
}

enum class Facing { BACK, FRONT, EXTERNAL }

/** Output formats a camera frame can be requested in, as relevant to UVC. */
enum class StreamFormat(val imageFormat: Int, val label: String) {
    /** Hardware-encoded JPEG, usable directly as MJPEG frames. */
    JPEG(ImageFormat.JPEG, "JPEG"),

    /** Raw frames for software MJPEG/YUYV encoding or MediaCodec input. */
    YUV(ImageFormat.YUV_420_888, "YUV_420_888"),
}

data class CameraInfo(
    val id: String,
    val facing: Facing,
    val hardwareLevel: String,
    /** Clockwise rotation of the sensor image relative to the phone's natural (portrait) orientation. */
    val sensorOrientation: Int,
    /** Per format, sizes that can sustain [TARGET_FRAME_DURATION_NS], largest first. */
    val streamableSizes: Map<StreamFormat, List<Resolution>>,
    /** CONTROL_ZOOM_RATIO_RANGE, e.g. 0.6 to 10 on a logical multi-camera. */
    val zoomRatioRange: Pair<Float, Float>? = null,
    /** Nearest focus distance in diopters; 0 for fixed focus. */
    val minFocusDistance: Float = 0f,
    val afModes: List<Int> = emptyList(),
    val awbModes: List<Int> = emptyList(),
    val antibandingModes: List<Int> = emptyList(),
)

/** 30 fps, the baseline frame rate every advertised UVC frame size must meet. */
const val TARGET_FRAME_DURATION_NS = 1_000_000_000L / 30

/**
 * Filter [sizes] to those whose minimum frame duration allows [maxFrameDurationNs], ordered from
 * largest to smallest. A minimum duration of 0 means the camera did not report one, which the
 * Camera2 API documents as unsupported for repeating requests, so such sizes are dropped.
 */
fun streamableSizes(
    sizes: List<Resolution>,
    maxFrameDurationNs: Long,
    minFrameDurationNs: (Resolution) -> Long,
): List<Resolution> = sizes
    .filter { minFrameDurationNs(it) in 1..maxFrameDurationNs }
    .sortedByDescending { it.area }

private fun hardwareLevelName(level: Int?): String = when (level) {
    CameraMetadata.INFO_SUPPORTED_HARDWARE_LEVEL_LEGACY -> "LEGACY"
    CameraMetadata.INFO_SUPPORTED_HARDWARE_LEVEL_LIMITED -> "LIMITED"
    CameraMetadata.INFO_SUPPORTED_HARDWARE_LEVEL_FULL -> "FULL"
    CameraMetadata.INFO_SUPPORTED_HARDWARE_LEVEL_3 -> "LEVEL_3"
    CameraMetadata.INFO_SUPPORTED_HARDWARE_LEVEL_EXTERNAL -> "EXTERNAL"
    else -> "UNKNOWN($level)"
}

/** Query capabilities of all cameras. Does not require the CAMERA permission. */
fun queryCameras(manager: CameraManager): List<CameraInfo> = manager.cameraIdList.map { id ->
    val chars = manager.getCameraCharacteristics(id)
    val map = chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)

    val facing = when (chars.get(CameraCharacteristics.LENS_FACING)) {
        CameraMetadata.LENS_FACING_FRONT -> Facing.FRONT
        CameraMetadata.LENS_FACING_BACK -> Facing.BACK
        else -> Facing.EXTERNAL
    }

    val sizes = StreamFormat.entries.associateWith { format ->
        val outputSizes = map?.getOutputSizes(format.imageFormat).orEmpty()
            .associateBy { Resolution(it.width, it.height) }

        streamableSizes(outputSizes.keys.toList(), TARGET_FRAME_DURATION_NS) {
            map!!.getOutputMinFrameDuration(format.imageFormat, outputSizes.getValue(it))
        }
    }

    CameraInfo(
        id = id,
        facing = facing,
        hardwareLevel = hardwareLevelName(
            chars.get(CameraCharacteristics.INFO_SUPPORTED_HARDWARE_LEVEL)),
        sensorOrientation = chars.get(CameraCharacteristics.SENSOR_ORIENTATION) ?: 90,
        streamableSizes = sizes,
        zoomRatioRange = chars.get(CameraCharacteristics.CONTROL_ZOOM_RATIO_RANGE)
            ?.let { it.lower to it.upper },
        minFocusDistance = chars.get(CameraCharacteristics.LENS_INFO_MINIMUM_FOCUS_DISTANCE) ?: 0f,
        afModes = chars.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES)?.toList().orEmpty(),
        awbModes = chars.get(CameraCharacteristics.CONTROL_AWB_AVAILABLE_MODES)?.toList().orEmpty(),
        antibandingModes = chars.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_ANTIBANDING_MODES)
            ?.toList().orEmpty(),
    )
}

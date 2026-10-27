// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraMetadata
import android.os.Build
import kotlin.math.atan
import kotlin.math.hypot

/** 35 mm film diagonal, for crop factors and equivalent focal lengths. */
private const val FULL_FRAME_DIAGONAL_MM = 43.27

/**
 * Optical format "inch" per mm of sensor diagonal: a 1″ type sensor has a
 * 16 mm diagonal, a legacy of vidicon tubes.
 */
private const val MM_PER_OPTICAL_INCH = 16.0

/** Hardware facts about a camera for the Device page, and what follows from them. */
data class CameraDetails(
    /** Pixels the camera normally outputs (binned on quad-Bayer sensors). */
    val activeArray: Resolution? = null,
    /** Full resolution of an ultra-high-resolution sensor, if it reports one. */
    val maxResolution: Resolution? = null,
    /** Sensor width and height in mm. */
    val physicalSizeMm: Pair<Float, Float>? = null,
    val focalLengthsMm: List<Float> = emptyList(),
    val apertures: List<Float> = emptyList(),
    val ois: Boolean = false,
    val flash: Boolean = false,
    val autofocus: Boolean = false,
    /** Nearest focus distance in diopters; 0 for fixed focus. */
    val minFocusDiopters: Float = 0f,
    val isoRange: Pair<Int, Int>? = null,
    val exposureRangeNs: Pair<Long, Long>? = null,
    val capabilities: List<Int> = emptyList(),
    /** Cameras behind a logical multi-camera. */
    val physicalIds: List<String> = emptyList(),
) {
    val megapixels: Double? get() = activeArray?.let { it.area / 1e6 }
    val maxMegapixels: Double? get() = maxResolution?.let { it.area / 1e6 }

    val diagonalMm: Double? get() = physicalSizeMm?.let { (w, h) -> hypot(w.toDouble(), h.toDouble()) }

    /** Pixel pitch in µm, at full resolution. */
    val pixelSizeUm: Double?
        get() {
            val (w, _) = physicalSizeMm ?: return null
            val pixels = (maxResolution ?: activeArray)?.width ?: return null
            return w * 1000.0 / pixels
        }

    /** Optical format denominator: 1.56 for a "1/1.56″" sensor. */
    val opticalFormat: Double? get() = diagonalMm?.let { MM_PER_OPTICAL_INCH / it }

    val cropFactor: Double? get() = diagonalMm?.let { FULL_FRAME_DIAGONAL_MM / it }

    val focalLengthMm: Float? get() = focalLengthsMm.minOrNull()

    /** 35 mm equivalent focal length. */
    val equivalentFocalLengthMm: Double?
        get() {
            val f = focalLengthMm ?: return null
            return cropFactor?.let { f * it }
        }

    /**
     * Horizontal and vertical field of view in degrees for an image of
     * [aspectWidth]:[aspectHeight] cropped from the center of the sensor,
     * as the camera does for video (16:9 from a 4:3 sensor keeps the width).
     */
    fun fieldOfView(aspectWidth: Int, aspectHeight: Int): Pair<Double, Double>? {
        val (sw, sh) = physicalSizeMm ?: return null
        val f = focalLengthMm?.takeIf { it > 0 } ?: return null
        val aspect = aspectWidth.toDouble() / aspectHeight
        val (w, h) = if (sw / sh > aspect) sh * aspect to sh.toDouble() else sw.toDouble() to sw / aspect
        fun angle(size: Double) = Math.toDegrees(2 * atan(size / (2 * f)))
        return angle(w) to angle(h)
    }

    /** Nearest focus distance in cm, if the lens can focus. */
    val minFocusCm: Double? get() = minFocusDiopters.takeIf { it > 0 }?.let { 100.0 / it }
}

/** Read [CameraDetails] from a camera's characteristics. */
fun cameraDetails(chars: CameraCharacteristics): CameraDetails {
    val capabilities = chars.get(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES)?.toList().orEmpty()
    val maxResolution = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S &&
        CameraMetadata.REQUEST_AVAILABLE_CAPABILITIES_ULTRA_HIGH_RESOLUTION_SENSOR in capabilities
    ) {
        chars.get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE_MAXIMUM_RESOLUTION)
            ?.let { Resolution(it.width(), it.height()) }
    } else {
        null
    }
    val afModes = chars.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES)?.toList().orEmpty()

    return CameraDetails(
        activeArray = chars.get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE)
            ?.let { Resolution(it.width(), it.height()) },
        maxResolution = maxResolution,
        physicalSizeMm = chars.get(CameraCharacteristics.SENSOR_INFO_PHYSICAL_SIZE)?.let { it.width to it.height },
        focalLengthsMm = chars.get(CameraCharacteristics.LENS_INFO_AVAILABLE_FOCAL_LENGTHS)?.toList().orEmpty(),
        apertures = chars.get(CameraCharacteristics.LENS_INFO_AVAILABLE_APERTURES)?.toList().orEmpty(),
        ois = CameraMetadata.LENS_OPTICAL_STABILIZATION_MODE_ON in
            chars.get(CameraCharacteristics.LENS_INFO_AVAILABLE_OPTICAL_STABILIZATION)?.toList().orEmpty(),
        flash = chars.get(CameraCharacteristics.FLASH_INFO_AVAILABLE) == true,
        autofocus = afModes.any { it != CameraMetadata.CONTROL_AF_MODE_OFF },
        minFocusDiopters = chars.get(CameraCharacteristics.LENS_INFO_MINIMUM_FOCUS_DISTANCE) ?: 0f,
        isoRange = chars.get(CameraCharacteristics.SENSOR_INFO_SENSITIVITY_RANGE)?.let { it.lower to it.upper },
        exposureRangeNs = chars.get(CameraCharacteristics.SENSOR_INFO_EXPOSURE_TIME_RANGE)?.let { it.lower to it.upper },
        capabilities = capabilities,
        physicalIds = if (CameraMetadata.REQUEST_AVAILABLE_CAPABILITIES_LOGICAL_MULTI_CAMERA in capabilities) {
            chars.physicalCameraIds.sorted()
        } else {
            emptyList()
        },
    )
}

/** An exposure time as photographers write it: "1/11760 s" below a second, else seconds. */
fun formatExposure(ns: Long): String =
    if (ns >= 1_000_000_000L) "%.0f s".format(ns / 1e9) else "1/${Math.round(1e9 / ns)} s"

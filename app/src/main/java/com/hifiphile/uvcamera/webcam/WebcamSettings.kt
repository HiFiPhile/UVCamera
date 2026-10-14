// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.webcam

import android.content.Context
import androidx.core.content.edit
import com.hifiphile.uvcamera.camera.CameraInfo
import com.hifiphile.uvcamera.camera.Facing

/** Largest frame height to advertise to the host. */
enum class MaxResolution(val height: Int, val label: String) {
    P1080(1080, "1080p"),
    P720(720, "720p"),
    P480(480, "480p"),
}

/** User choices for the webcam, applied the next time it's enabled. */
class WebcamSettings(context: Context) {
    private val prefs = context.getSharedPreferences("webcam", Context.MODE_PRIVATE)

    /** Camera to use; null for the first back camera. */
    var cameraId: String?
        get() = prefs.getString(KEY_CAMERA_ID, null)
        set(value) = prefs.edit { putString(KEY_CAMERA_ID, value) }

    /** Rotate the image by 180° on top of the per-camera correction (upside-down mount). */
    var rotate180: Boolean
        get() = prefs.getBoolean(KEY_ROTATE_180, false)
        set(value) = prefs.edit { putBoolean(KEY_ROTATE_180, value) }

    var maxResolution: MaxResolution
        get() = prefs.getString(KEY_MAX_RESOLUTION, null)
            ?.let { name -> MaxResolution.entries.find { it.name == name } }
            ?: MaxResolution.P1080
        set(value) = prefs.edit { putString(KEY_MAX_RESOLUTION, value.name) }

    companion object {
        private const val KEY_CAMERA_ID = "camera_id"
        private const val KEY_MAX_RESOLUTION = "max_resolution"
        private const val KEY_ROTATE_180 = "rotate_180"

        /**
         * Clockwise rotation that makes [camera]'s frames upright with the
         * phone in landscape, top to the left. Turning the phone looks
         * opposite to front and back cameras, so the same physical position
         * needs `sensor - 90` for back cameras and `sensor + 90` for front
         * ones: 0 for the usual 90° back and 270° front sensors.
         */
        fun landscapeRotation(camera: CameraInfo): Int = when (camera.facing) {
            Facing.BACK -> (camera.sensorOrientation + 270) % 360
            Facing.FRONT -> (camera.sensorOrientation + 90) % 360
            // External cameras deliver upright images.
            Facing.EXTERNAL -> 0
        }

        /**
         * Whether frames from [camera] need a 180° turn: its landscape
         * correction, flipped again for [userRotate180]. Only 0° and 180°
         * are supported (a 90° turn wouldn't fit a landscape frame), which
         * covers sensors mounted at 90° or 270°.
         */
        fun needsRotate180(camera: CameraInfo, userRotate180: Boolean): Boolean =
            (landscapeRotation(camera) == 180) != userRotate180

        /** The chosen camera, or the first back camera, or any camera. */
        fun pickCamera(cameras: List<CameraInfo>, cameraId: String?): CameraInfo? =
            cameras.find { it.id == cameraId }
                ?: cameras.firstOrNull { it.facing == Facing.BACK }
                ?: cameras.firstOrNull()
    }
}

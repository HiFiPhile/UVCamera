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

    var maxResolution: MaxResolution
        get() = prefs.getString(KEY_MAX_RESOLUTION, null)
            ?.let { name -> MaxResolution.entries.find { it.name == name } }
            ?: MaxResolution.P1080
        set(value) = prefs.edit { putString(KEY_MAX_RESOLUTION, value.name) }

    companion object {
        private const val KEY_CAMERA_ID = "camera_id"
        private const val KEY_MAX_RESOLUTION = "max_resolution"

        /** The chosen camera, or the first back camera, or any camera. */
        fun pickCamera(cameras: List<CameraInfo>, cameraId: String?): CameraInfo? =
            cameras.find { it.id == cameraId }
                ?: cameras.firstOrNull { it.facing == Facing.BACK }
                ?: cameras.firstOrNull()
    }
}

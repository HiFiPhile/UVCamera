// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.webcam

import android.content.Context
import android.content.SharedPreferences
import androidx.core.content.edit
import com.hifiphile.uvcamera.camera.CameraInfo
import com.hifiphile.uvcamera.camera.Facing
import com.hifiphile.uvcamera.camera.Rotation
import com.hifiphile.uvcamera.camera.Transform

/** Largest frame height to advertise to the host. */
enum class MaxResolution(val height: Int, val label: String) {
    P1080(1080, "1080p"),
    P720(720, "720p"),
    P480(480, "480p"),
}

/** User choices for the webcam, applied live by the running service. */
class WebcamSettings(context: Context) {
    private val prefs = context.getSharedPreferences("webcam", Context.MODE_PRIVATE)

    /** Camera to use; null for the first back camera. */
    var cameraId: String?
        get() = prefs.getString(KEY_CAMERA_ID, null)
        set(value) = prefs.edit { putString(KEY_CAMERA_ID, value) }

    /**
     * User rotation, clockwise, on top of the per-camera landscape
     * correction. Earlier versions only had a 180° toggle.
     */
    var rotation: Rotation
        get() = when {
            prefs.contains(KEY_ROTATION) ->
                runCatching { Rotation.fromDegrees(prefs.getInt(KEY_ROTATION, 0)) }
                    .getOrDefault(Rotation.R0)
            prefs.getBoolean(KEY_ROTATE_180_LEGACY, false) -> Rotation.R180
            else -> Rotation.R0
        }
        set(value) = prefs.edit {
            putInt(KEY_ROTATION, value.degrees)
            remove(KEY_ROTATE_180_LEGACY)
        }

    /** Flip the image left/right. */
    var mirrorH: Boolean
        get() = prefs.getBoolean(KEY_MIRROR_H, false)
        set(value) = prefs.edit { putBoolean(KEY_MIRROR_H, value) }

    /** Flip the image top/bottom. */
    var mirrorV: Boolean
        get() = prefs.getBoolean(KEY_MIRROR_V, false)
        set(value) = prefs.edit { putBoolean(KEY_MIRROR_V, value) }

    /** Listen for changes that can be applied while streaming. */
    fun onTransformChanged(listener: () -> Unit): SharedPreferences.OnSharedPreferenceChangeListener {
        val l = SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
            if (key in TRANSFORM_KEYS) listener()
        }
        prefs.registerOnSharedPreferenceChangeListener(l)
        return l
    }

    /** Listen for camera or resolution cap changes. */
    fun onSourceChanged(listener: () -> Unit): SharedPreferences.OnSharedPreferenceChangeListener {
        val l = SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
            if (key in SOURCE_KEYS) listener()
        }
        prefs.registerOnSharedPreferenceChangeListener(l)
        return l
    }

    fun removeListener(listener: SharedPreferences.OnSharedPreferenceChangeListener) =
        prefs.unregisterOnSharedPreferenceChangeListener(listener)

    var maxResolution: MaxResolution
        get() = prefs.getString(KEY_MAX_RESOLUTION, null)
            ?.let { name -> MaxResolution.entries.find { it.name == name } }
            ?: MaxResolution.P1080
        set(value) = prefs.edit { putString(KEY_MAX_RESOLUTION, value.name) }

    companion object {
        private const val KEY_CAMERA_ID = "camera_id"
        private const val KEY_MAX_RESOLUTION = "max_resolution"
        private const val KEY_ROTATE_180_LEGACY = "rotate_180"
        private const val KEY_ROTATION = "rotation"
        private const val KEY_MIRROR_H = "mirror_h"
        private const val KEY_MIRROR_V = "mirror_v"
        private val TRANSFORM_KEYS = setOf(KEY_ROTATION, KEY_MIRROR_H, KEY_MIRROR_V)
        private val SOURCE_KEYS = setOf(KEY_CAMERA_ID, KEY_MAX_RESOLUTION)

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
         * The full transform for [camera]: its landscape correction plus the
         * user's rotation, then the user's mirroring of the final image.
         */
        fun transformFor(camera: CameraInfo, rotation: Rotation, mirrorH: Boolean, mirrorV: Boolean) =
            Transform(Rotation.fromDegrees(landscapeRotation(camera)) + rotation, mirrorH, mirrorV)

        /** The chosen camera, or the first back camera, or any camera. */
        fun pickCamera(cameras: List<CameraInfo>, cameraId: String?): CameraInfo? =
            cameras.find { it.id == cameraId }
                ?: cameras.firstOrNull { it.facing == Facing.BACK }
                ?: cameras.firstOrNull()
    }
}

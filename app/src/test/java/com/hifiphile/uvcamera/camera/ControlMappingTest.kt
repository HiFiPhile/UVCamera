// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import android.hardware.camera2.CameraMetadata
import com.hifiphile.uvcamera.daemon.ControlCaps
import com.hifiphile.uvcamera.webcam.AWB_PRESET_KELVIN
import com.hifiphile.uvcamera.webcam.webcamControls
import org.junit.Assert.assertEquals
import org.junit.Test

class ControlMappingTest {
    @Test
    fun zoomIsHundredthsClampedToTheCameraRange() {
        assertEquals(1f, CameraStreamer.zoomRatio(100, 0.6f, 10f))
        assertEquals(0.6f, CameraStreamer.zoomRatio(50, 0.6f, 10f))
        assertEquals(10f, CameraStreamer.zoomRatio(2000, 0.6f, 10f))
    }

    @Test
    fun focusRunsFromNearestToInfinity() {
        assertEquals(10f, CameraStreamer.focusDiopters(0, 10f))
        assertEquals(0f, CameraStreamer.focusDiopters(1000, 10f))
        assertEquals(5f, CameraStreamer.focusDiopters(500, 10f))
    }

    @Test
    fun whiteBalancePicksTheNearestAvailablePreset() {
        val all = AWB_PRESET_KELVIN.keys.toList()
        assertEquals(CameraMetadata.CONTROL_AWB_MODE_INCANDESCENT,
            CameraStreamer.awbPreset(2800, all, AWB_PRESET_KELVIN))
        assertEquals(CameraMetadata.CONTROL_AWB_MODE_DAYLIGHT,
            CameraStreamer.awbPreset(5000, all, AWB_PRESET_KELVIN))
        // Without the exact presets, the closest available one.
        val few = listOf(CameraMetadata.CONTROL_AWB_MODE_FLUORESCENT, CameraMetadata.CONTROL_AWB_MODE_SHADE)
        assertEquals(CameraMetadata.CONTROL_AWB_MODE_SHADE,
            CameraStreamer.awbPreset(6500, few, AWB_PRESET_KELVIN))
        assertEquals(CameraMetadata.CONTROL_AWB_MODE_AUTO,
            CameraStreamer.awbPreset(5000, emptyList(), AWB_PRESET_KELVIN))
    }

    @Test
    fun antibandingFallsBackToAuto() {
        val modes = listOf(
            CameraMetadata.CONTROL_AE_ANTIBANDING_MODE_OFF,
            CameraMetadata.CONTROL_AE_ANTIBANDING_MODE_50HZ,
            CameraMetadata.CONTROL_AE_ANTIBANDING_MODE_AUTO,
        )
        assertEquals(CameraMetadata.CONTROL_AE_ANTIBANDING_MODE_50HZ, CameraStreamer.antibanding(1, modes))
        assertEquals(CameraMetadata.CONTROL_AE_ANTIBANDING_MODE_AUTO, CameraStreamer.antibanding(2, modes))
        assertEquals(CameraMetadata.CONTROL_AE_ANTIBANDING_MODE_OFF, CameraStreamer.antibanding(0, modes))
    }

    @Test
    fun capsFromCameraCharacteristics() {
        val camera = CameraInfo(
            id = "0", facing = Facing.BACK, hardwareLevel = "LEVEL_3", sensorOrientation = 90,
            streamableSizes = emptyMap(),
            zoomRatioRange = 0.6f to 10f,
            minFocusDistance = 10f,
            afModes = listOf(CameraMetadata.CONTROL_AF_MODE_OFF, CameraMetadata.CONTROL_AF_MODE_CONTINUOUS_VIDEO),
            awbModes = listOf(CameraMetadata.CONTROL_AWB_MODE_AUTO, CameraMetadata.CONTROL_AWB_MODE_DAYLIGHT,
                CameraMetadata.CONTROL_AWB_MODE_INCANDESCENT),
            antibandingModes = listOf(CameraMetadata.CONTROL_AE_ANTIBANDING_MODE_AUTO,
                CameraMetadata.CONTROL_AE_ANTIBANDING_MODE_50HZ),
        )
        assertEquals(ControlCaps(60 to 1000, focus = true, whiteBalance = true, powerLine = true),
            webcamControls(camera))

        // Fixed-focus front camera without zoom or presets.
        val front = camera.copy(zoomRatioRange = 1f to 1f, minFocusDistance = 0f,
            awbModes = listOf(CameraMetadata.CONTROL_AWB_MODE_AUTO), antibandingModes = emptyList())
        assertEquals(ControlCaps(), webcamControls(front))

        // No presets, but colour gains can be set from the calibration.
        assertEquals(ControlCaps(whiteBalance = true), webcamControls(front.copy(manualWhiteBalance = true)))

        // Switchable cameras advertise the union, zoom over both ranges.
        val front10x = front.copy(zoomRatioRange = 1f to 10f)
        assertEquals(ControlCaps(60 to 1000, focus = true, whiteBalance = true, powerLine = true),
            webcamControls(listOf(front10x, camera.copy(zoomRatioRange = 0.6f to 2f))))
        assertEquals(ControlCaps(), webcamControls(emptyList()))
    }
}

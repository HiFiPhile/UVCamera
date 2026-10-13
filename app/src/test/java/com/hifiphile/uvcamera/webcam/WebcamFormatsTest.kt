// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.webcam

import com.hifiphile.uvcamera.camera.CameraInfo
import com.hifiphile.uvcamera.camera.Facing
import com.hifiphile.uvcamera.camera.Resolution
import com.hifiphile.uvcamera.camera.StreamFormat
import com.hifiphile.uvcamera.daemon.FormatKind
import com.hifiphile.uvcamera.daemon.FormatSpec
import org.junit.Assert.assertEquals
import org.junit.Test

class WebcamFormatsTest {
    private fun camera(jpeg: List<Resolution>, yuv: List<Resolution>) = CameraInfo(
        id = "0",
        facing = Facing.BACK,
        hardwareLevel = "FULL",
        streamableSizes = mapOf(StreamFormat.JPEG to jpeg, StreamFormat.YUV to yuv),
    )

    @Test
    fun onePlus15BackCamera() {
        // JPEG only from 1080p up, YUV at every standard size.
        val formats = webcamFormats(camera(
            jpeg = listOf(Resolution(3840, 2160), Resolution(1920, 1080)),
            yuv = listOf(Resolution(1920, 1080), Resolution(1280, 720), Resolution(640, 480)),
        ))

        assertEquals(listOf(FormatKind.MJPEG, FormatKind.YUYV), formats.map { it.kind })
        assertEquals(
            listOf(1920 to 1080, 1280 to 720, 640 to 480),
            formats[0].frames.map { it.width to it.height },
        )
        assertEquals(listOf(333_333, 666_666), formats[0].frames[0].intervals)
        assertEquals(listOf(640 to 480, 1280 to 720), formats[1].frames.map { it.width to it.height })
    }

    @Test
    fun cameraWithoutYuvOnlyGetsHardwareJpegSizes() {
        val formats = webcamFormats(camera(jpeg = listOf(Resolution(1280, 720)), yuv = emptyList()))
        assertEquals(1, formats.size)
        assertEquals(listOf(1280 to 720), formats[0].frames.map { it.width to it.height })
    }

    @Test
    fun nothingUsableFallsBackToDefaults() {
        assertEquals(FormatSpec.defaults(), webcamFormats(camera(emptyList(), emptyList())))
    }
}

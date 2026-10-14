// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.webcam

import com.hifiphile.uvcamera.camera.CameraInfo
import com.hifiphile.uvcamera.camera.Rotation
import com.hifiphile.uvcamera.camera.Transform
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
        sensorOrientation = 90,
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
    fun maxResolutionCapsBothFormats() {
        val formats = webcamFormats(
            camera(
                jpeg = listOf(Resolution(1920, 1080)),
                yuv = listOf(Resolution(1920, 1080), Resolution(1280, 720), Resolution(640, 480)),
            ),
            maxHeight = 720,
        )
        assertEquals(listOf(1280 to 720, 640 to 480), formats[0].frames.map { it.width to it.height })
        assertEquals(listOf(640 to 480, 1280 to 720), formats[1].frames.map { it.width to it.height })

        val vga = webcamFormats(camera(jpeg = emptyList(), yuv = listOf(Resolution(1280, 720), Resolution(640, 480))), maxHeight = 480)
        assertEquals(listOf(listOf(640 to 480), listOf(640 to 480)), vga.map { f -> f.frames.map { it.width to it.height } })
    }

    @Test
    fun pickCameraPrefersChoiceThenBack() {
        val back = camera(emptyList(), emptyList())
        val front = back.copy(id = "1", facing = Facing.FRONT)
        assertEquals("1", WebcamSettings.pickCamera(listOf(back, front), "1")?.id)
        assertEquals("0", WebcamSettings.pickCamera(listOf(front, back), null)?.id)
        assertEquals("0", WebcamSettings.pickCamera(listOf(front, back), "9")?.id)
        assertEquals("1", WebcamSettings.pickCamera(listOf(front), null)?.id)
    }

    @Test
    fun rotationDependsOnFacingThenAppliesUserFlip() {
        val back = camera(emptyList(), emptyList())
        val front = back.copy(id = "1", facing = Facing.FRONT, sensorOrientation = 270)
        // OnePlus 15 (back 90°, front 270°): both upright in the same landscape
        // position. The front camera's 0° was confirmed on the device (a 180°
        // turn came out upside down).
        assertEquals(0, WebcamSettings.landscapeRotation(back))
        assertEquals(0, WebcamSettings.landscapeRotation(front))
        fun t(c: CameraInfo, r: Rotation, h: Boolean = false) = WebcamSettings.transformFor(c, r, h, false)
        assertEquals(Transform(), t(back, Rotation.R0))
        assertEquals(Transform(), t(front, Rotation.R0))
        assertEquals(Transform(Rotation.R90, mirrorH = true), t(front, Rotation.R90, h = true))

        // Sensors mounted the other way (e.g. the Nexus 5X back camera at 270°):
        // the correction adds to the user's rotation.
        val flippedBack = back.copy(sensorOrientation = 270)
        val flippedFront = front.copy(sensorOrientation = 90)
        assertEquals(Transform(Rotation.R180), t(flippedBack, Rotation.R0))
        assertEquals(Transform(Rotation.R180), t(flippedFront, Rotation.R0))
        assertEquals(Transform(Rotation.R0), t(flippedBack, Rotation.R180))
        assertEquals(Transform(Rotation.R270), t(flippedBack, Rotation.R90))
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

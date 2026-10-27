// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class CapturePlanTest {
    /** OnePlus 15 back camera YUV sizes at >= 30 fps. */
    private val sizes = listOf(
        4096 to 3072, 3840 to 2160, 3280 to 2464, 3840 to 1644, 3840 to 1608, 3280 to 1856,
        3264 to 1836, 2560 to 1920, 3136 to 1440, 2560 to 1440, 2772 to 1272, 2580 to 1080,
        1920 to 1440, 2520 to 1080, 2352 to 1080, 1920 to 1296, 2160 to 1080, 1920 to 1080,
        1600 to 1200, 1664 to 936, 1440 to 1080, 1200 to 1200, 1600 to 800, 1280 to 960,
        1080 to 1080, 1568 to 720, 1280 to 720, 1024 to 768, 1176 to 540, 864 to 480,
        720 to 540, 720 to 480, 800 to 400, 640 to 480, 784 to 360, 640 to 360,
    )

    private fun choose(w: Int, h: Int, r: Rotation) =
        CameraStreamer.chooseCaptureSize(sizes, w, h, Transform(r))

    @Test
    fun uprightUsesTheOutputSize() {
        assertEquals(1920 to 1080, choose(1920, 1080, Rotation.R0))
        assertEquals(1280 to 720, choose(1280, 720, Rotation.R180))
    }

    @Test
    fun quarterTurnsCaptureASizeWhoseShortSideCoversTheWidth() {
        assertEquals(2560 to 1920, choose(1920, 1080, Rotation.R90))
        // 1920x1296 rotated is 1296 wide, just enough for 1280.
        assertEquals(1920 to 1296, choose(1280, 720, Rotation.R270))
        assertEquals(1024 to 768, choose(640, 480, Rotation.R90))
    }

    @Test
    fun withoutACoveringSizeTheLargestCoverageWins() {
        // OnePlus 15 front camera YUV sizes at 60 fps: none has a short side
        // of 1920, so rotated 1080p gets 1856 columns and black bars.
        val front60 = listOf(
            3280 to 1856, 3264 to 1836, 3136 to 1440, 2772 to 1272, 2580 to 1080,
            2560 to 1440, 2520 to 1080, 2352 to 1080, 2160 to 1080, 1920 to 1080,
        )
        val r90 = Transform(Rotation.R90)
        assertEquals(3280 to 1856, CameraStreamer.chooseCaptureSize(front60, 1920, 1080, r90))
        assertEquals(1856.0 / 1920, CameraStreamer.coverage(3280, 1856, 1920, 1080, r90), 1e-9)
        // Upright, 1920x1080 itself covers it.
        assertEquals(1920 to 1080, CameraStreamer.chooseCaptureSize(front60, 1920, 1080, Transform()))
        // A camera that only reaches 720p is centered in 1080p.
        assertEquals(1280 to 720, CameraStreamer.chooseCaptureSize(listOf(1280 to 720, 640 to 480), 1920, 1080, Transform()))
        assertNull(CameraStreamer.chooseCaptureSize(emptyList(), 1920, 1080, r90))
    }

    @Test
    fun frameDurationAllowsTruncatedIntervals() {
        // 30 fps: UVC says 333333 x 100 ns, cameras say 33333333 ns.
        assertEquals(true, 33_333_333L <= CameraStreamer.maxFrameDurationNs(333_333))
        assertEquals(false, 33_333_334L + 100 <= CameraStreamer.maxFrameDurationNs(333_333))
        assertEquals(true, 66_666_666L <= CameraStreamer.maxFrameDurationNs(666_666))
    }
}

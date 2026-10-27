// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class CameraDetailsTest {
    // OnePlus 15 cameras, from their characteristics.
    private val back = CameraDetails(
        activeArray = Resolution(4096, 3072),
        physicalSizeMm = 8.192f to 6.144f,
        focalLengthsMm = listOf(5.59f),
        apertures = listOf(1.88f),
        minFocusDiopters = 10f,
    )
    private val front = CameraDetails(
        activeArray = Resolution(3280, 2464),
        physicalSizeMm = 5.248f to 3.9424f,
        focalLengthsMm = listOf(3.23f),
    )

    @Test
    fun sensor() {
        assertEquals(12.58, back.megapixels!!, 0.01)
        assertEquals(10.24, back.diagonalMm!!, 0.01)
        // 1/1.56″: 16 mm per optical inch.
        assertEquals(1.5625, back.opticalFormat!!, 0.001)
        assertEquals(2.0, back.pixelSizeUm!!, 0.001)
        assertEquals(2.44, front.opticalFormat!!, 0.01)
        assertEquals(1.6, front.pixelSizeUm!!, 0.001)

        // Quad-Bayer: pixel pitch at full resolution.
        val quad = back.copy(maxResolution = Resolution(8192, 6144))
        assertEquals(50.3, quad.maxMegapixels!!, 0.1)
        assertEquals(1.0, quad.pixelSizeUm!!, 0.001)
    }

    @Test
    fun lens() {
        assertEquals(23.6, back.equivalentFocalLengthMm!!, 0.1)
        assertEquals(21.3, front.equivalentFocalLengthMm!!, 0.1)
        assertEquals(10.0, back.minFocusCm!!, 1e-9)
        assertNull(front.minFocusCm)
    }

    @Test
    fun fieldOfViewOfA16By9CropKeepsTheWidth() {
        val (h, v) = back.fieldOfView(16, 9)!!
        assertEquals(72.5, h, 0.1)
        assertEquals(44.8, v, 0.1)
        // 4:3 is the whole sensor.
        val (h43, v43) = back.fieldOfView(4, 3)!!
        assertEquals(72.5, h43, 0.1)
        assertEquals(57.6, v43, 0.1)
        // A sensor wider than 16:9 keeps its height instead.
        val wide = back.copy(physicalSizeMm = 8.192f to 3.0f)
        assertEquals(wide.fieldOfView(16, 9)!!.second, CameraDetails(physicalSizeMm = 1f to 3.0f, focalLengthsMm = listOf(5.59f)).fieldOfView(1, 3)!!.second, 0.01)
        assertNull(CameraDetails().fieldOfView(16, 9))
    }

    @Test
    fun exposureTimes() {
        assertEquals("1/11760 s", formatExposure(85_034))
        assertEquals("1/30 s", formatExposure(33_333_333))
        assertEquals("55 s", formatExposure(54_525_757_487))
    }
}

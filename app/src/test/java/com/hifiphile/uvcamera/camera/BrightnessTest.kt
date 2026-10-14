// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import org.junit.Assert.assertEquals
import org.junit.Test

class BrightnessTest {
    private fun map(b: Int) = CameraStreamer.brightnessToCompensation(b, -12, 12)

    @Test
    fun defaultIsNoCompensation() = assertEquals(0, map(128))

    @Test
    fun endsReachTheRangeLimits() {
        assertEquals(12, map(255))
        assertEquals(-12, map(0))
    }

    @Test
    fun linearOnEachSide() {
        assertEquals(6, map(192))
        assertEquals(-6, map(64))
    }

    @Test
    fun asymmetricRangeAndClamping() {
        assertEquals(4, CameraStreamer.brightnessToCompensation(255, -8, 4))
        assertEquals(-8, CameraStreamer.brightnessToCompensation(-5, -8, 4))
        assertEquals(0, CameraStreamer.brightnessToCompensation(128, 0, 0))
    }
}

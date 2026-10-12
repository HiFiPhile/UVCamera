// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import org.junit.Assert.assertEquals
import org.junit.Test

class CameraCapabilitiesTest {
    @Test
    fun streamableSizesFiltersByFrameDurationAndSortsLargestFirst() {
        val vga = Resolution(640, 480)
        val hd = Resolution(1280, 720)
        val fhd = Resolution(1920, 1080)
        val uhd = Resolution(3840, 2160)
        val unreported = Resolution(320, 240)

        val durations = mapOf(
            vga to 33_333_333L,
            hd to 16_666_666L,
            fhd to TARGET_FRAME_DURATION_NS,
            uhd to 50_000_000L,
            unreported to 0L,
        )

        val result = streamableSizes(
            listOf(vga, uhd, unreported, fhd, hd),
            TARGET_FRAME_DURATION_NS,
            durations::getValue,
        )

        assertEquals(listOf(fhd, hd, vga), result)
    }
}

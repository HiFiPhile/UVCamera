// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class WhiteBalanceTest {
    private val identity = doubleArrayOf(1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0)

    @Test
    fun whitePointsOfStandardIlluminants() {
        val (x65, y65) = WhiteBalance.illuminantXy(6504.0)
        assertEquals(0.3127, x65, 0.002)
        assertEquals(0.3290, y65, 0.002)
        val (xa, ya) = WhiteBalance.illuminantXy(2856.0)
        assertEquals(0.4476, xa, 0.003)
        assertEquals(0.4074, ya, 0.003)
    }

    @Test
    fun withIdentityCalibrationGainsInvertTheWhitePoint() {
        val g = WhiteBalance.gains(6504.0, 6504.0, identity, identity, 2856.0, identity, identity)
        val (x, y) = WhiteBalance.illuminantXy(6504.0)
        assertEquals(y / x, g[0], 1e-9)
        assertEquals(1.0, g[1], 0.0)
        assertEquals(y / (1 - x - y), g[2], 1e-9)
    }

    @Test
    fun warmerLightNeedsLessRedAndMoreBlueGain() {
        val warm = WhiteBalance.gains(2800.0, 6504.0, identity, identity, 2856.0, identity, identity)
        val cool = WhiteBalance.gains(6500.0, 6504.0, identity, identity, 2856.0, identity, identity)
        assertTrue(warm[0] < cool[0])
        assertTrue(warm[2] > cool[2])
    }

    @Test
    fun interpolatesBetweenReferenceMatricesByInverseTemperature() {
        // Reference 1 scales camera red by 2, reference 2 by 1. Halfway in
        // mired between 6504 K and 2856 K, red sensitivity is 1.5x.
        val red2 = doubleArrayOf(2.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0)
        val mid = 1.0 / ((1 / 6504.0 + 1 / 2856.0) / 2)
        val g = WhiteBalance.gains(mid, 6504.0, red2, identity, 2856.0, identity, identity)
        val plain = WhiteBalance.gains(mid, 6504.0, identity, identity, 2856.0, identity, identity)
        assertEquals(plain[0] / 1.5, g[0], 1e-9)
        // Outside the calibrated range the nearest reference applies.
        val hot = WhiteBalance.gains(10000.0, 6504.0, red2, identity, 2856.0, identity, identity)
        val hotPlain = WhiteBalance.gains(10000.0, 6504.0, identity, identity, 2856.0, identity, identity)
        assertEquals(hotPlain[0] / 2, hot[0], 1e-9)
    }

    @Test
    fun forwardMatrixOfSrgbPrimariesGivesIdentityCorrection() {
        // sRGB to XYZ D50 (Bradford): a camera with sRGB primaries needs no correction.
        val srgbToXyzD50 = doubleArrayOf(
            0.4360747, 0.3850649, 0.1430804,
            0.2225045, 0.7168786, 0.0606169,
            0.0139322, 0.0971045, 0.7141733,
        )
        val ccm = WhiteBalance.colorTransform(5000.0, 6504.0, srgbToXyzD50, 2856.0, srgbToXyzD50)
        for (i in 0 until 9) assertEquals(identity[i], ccm[i], 1e-4)
    }

    @Test
    fun calibrationThatWhitensEachReferenceIsDetected() {
        // Colour matrices scaled so each reference's white maps to (1, 1, 1).
        fun whitening(kelvin: Double): DoubleArray {
            val (x, y) = WhiteBalance.illuminantXy(kelvin)
            return doubleArrayOf(y / x, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, y / (1 - x - y))
        }
        val whitened = ColorCalibration(6504.0, whitening(6504.0), identity, identity,
            2856.0, whitening(2856.0), identity, identity)
        assertEquals(1.0, whitened.gains(4000.0)[0], 0.05)
        assertEquals(false, whitened.distinguishesIlluminants)

        val real = ColorCalibration(6504.0, identity, identity, identity, 2856.0, identity, identity, identity)
        assertEquals(true, real.distinguishesIlluminants)
    }
}

// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

/**
 * Manual white balance from a colour temperature, using the camera's own
 * colour calibration the way DNG processing does: interpolate the two
 * reference-illuminant matrices by inverse temperature, map the white point
 * of the light into camera space, and invert it to get per-channel gains.
 */
object WhiteBalance {
    /**
     * CIE xy chromaticity of the white point of a light at [kelvin]: the CIE
     * daylight locus from 4000 K, the Planckian locus (Kim et al. cubic
     * approximation) below.
     */
    fun illuminantXy(kelvin: Double): Pair<Double, Double> {
        val t = kelvin.coerceIn(1667.0, 25000.0)
        if (t >= 4000.0) {
            val x = if (t <= 7000.0) {
                -4.6070e9 / (t * t * t) + 2.9678e6 / (t * t) + 0.09911e3 / t + 0.244063
            } else {
                -2.0064e9 / (t * t * t) + 1.9018e6 / (t * t) + 0.24748e3 / t + 0.237040
            }
            val y = -3.000 * x * x + 2.870 * x - 0.275
            return x to y
        }

        val x = -0.2661239e9 / (t * t * t) - 0.2343589e6 / (t * t) + 0.8776956e3 / t + 0.179910
        val y = if (t <= 2222.0) {
            -1.1063814 * x * x * x - 1.34811020 * x * x + 2.18555832 * x - 0.20219683
        } else {
            -0.9549476 * x * x * x - 1.37418593 * x * x + 2.09137015 * x - 0.16748867
        }
        return x to y
    }

    /** Temperature of an EXIF/DNG LightSource code, for the codes cameras use as references. */
    fun referenceKelvin(lightSource: Int): Double? = when (lightSource) {
        17 -> 2856.0 // Standard light A
        18 -> 4874.0 // Standard light B
        19 -> 6774.0 // Standard light C
        20 -> 5503.0 // D55
        21 -> 6504.0 // D65
        22 -> 7504.0 // D75
        23 -> 5003.0 // D50
        24 -> 3200.0 // ISO studio tungsten
        3 -> 2850.0 // Tungsten
        14 -> 4150.0 // Cool white fluorescent
        else -> null
    }

    /** Row-major 3x3 matrix times a 3-vector. */
    private fun matVec(m: DoubleArray, v: DoubleArray) = DoubleArray(3) { r ->
        m[r * 3] * v[0] + m[r * 3 + 1] * v[1] + m[r * 3 + 2] * v[2]
    }

    /** Row-major 3x3 matrix product. */
    private fun matMul(a: DoubleArray, b: DoubleArray) =
        DoubleArray(9) { i ->
            val r = i / 3
            val c = i % 3
            a[r * 3] * b[c] + a[r * 3 + 1] * b[3 + c] + a[r * 3 + 2] * b[6 + c]
        }

    /**
     * Weight of reference 1 for a light at [kelvin], linear in inverse
     * temperature (mired) and clamped to the calibrated range.
     */
    fun weight(kelvin: Double, kelvin1: Double, kelvin2: Double): Double =
        if (kelvin1 == kelvin2) 1.0
        else ((1.0 / kelvin - 1.0 / kelvin2) / (1.0 / kelvin1 - 1.0 / kelvin2)).coerceIn(0.0, 1.0)

    private fun lerp(w: Double, a: DoubleArray, b: DoubleArray) = DoubleArray(9) { w * a[it] + (1 - w) * b[it] }

    /**
     * Per-channel gains (red, green, blue; green = 1) that make a light at
     * [kelvin] neutral, from the camera's calibration: colour matrices
     * ([colorMatrix1]/[colorMatrix2], XYZ to camera) and calibration
     * transforms for reference illuminants at [kelvin1] and [kelvin2].
     * Matrices are row-major 3x3.
     */
    fun gains(
        kelvin: Double,
        kelvin1: Double,
        colorMatrix1: DoubleArray,
        calibration1: DoubleArray,
        kelvin2: Double,
        colorMatrix2: DoubleArray,
        calibration2: DoubleArray,
    ): DoubleArray {
        val m = lerp(weight(kelvin, kelvin1, kelvin2),
            matMul(calibration1, colorMatrix1), matMul(calibration2, colorMatrix2))
        val (x, y) = illuminantXy(kelvin)
        val xyz = doubleArrayOf(x / y, 1.0, (1 - x - y) / y)
        val neutral = matVec(m, xyz)
        return doubleArrayOf(neutral[1] / neutral[0], 1.0, neutral[1] / neutral[2])
    }

    /** Linear sRGB from XYZ relative to D50 (Bradford-adapted), row-major. */
    private val XYZ_D50_TO_SRGB = doubleArrayOf(
        3.1338561, -1.6168667, -0.4906146,
        -0.9787684, 1.9161415, 0.0334540,
        0.0719453, -0.2289914, 1.4052427,
    )

    /**
     * Colour correction matrix (white-balanced camera RGB to linear sRGB)
     * for a light at [kelvin], from the forward matrices (white-balanced
     * camera RGB to XYZ D50) of the two reference illuminants.
     */
    fun colorTransform(
        kelvin: Double,
        kelvin1: Double,
        forwardMatrix1: DoubleArray,
        kelvin2: Double,
        forwardMatrix2: DoubleArray,
    ): DoubleArray = matMul(XYZ_D50_TO_SRGB,
        lerp(weight(kelvin, kelvin1, kelvin2), forwardMatrix1, forwardMatrix2))
}

/**
 * A camera's colour calibration for two reference illuminants (the same
 * one twice if it only has one), as reported in its characteristics.
 * Matrices are row-major 3x3.
 */
class ColorCalibration(
    val kelvin1: Double,
    val colorMatrix1: DoubleArray,
    val calibration1: DoubleArray,
    val forwardMatrix1: DoubleArray,
    val kelvin2: Double,
    val colorMatrix2: DoubleArray,
    val calibration2: DoubleArray,
    val forwardMatrix2: DoubleArray,
) {
    fun gains(kelvin: Double) =
        WhiteBalance.gains(kelvin, kelvin1, colorMatrix1, calibration1, kelvin2, colorMatrix2, calibration2)

    fun colorTransform(kelvin: Double) =
        WhiteBalance.colorTransform(kelvin, kelvin1, forwardMatrix1, kelvin2, forwardMatrix2)

    /**
     * Whether the calibration tells the reference illuminants apart. Some
     * cameras report colour matrices that already map each reference's
     * white to a neutral camera response, which gives gains of 1 at every
     * temperature.
     */
    val distinguishesIlluminants: Boolean
        get() {
            val g1 = gains(kelvin1)
            val g2 = gains(kelvin2)
            return kelvin1 != kelvin2 &&
                (Math.abs(Math.log(g1[0] / g2[0])) > 0.1 || Math.abs(Math.log(g1[2] / g2[2])) > 0.1)
        }
}

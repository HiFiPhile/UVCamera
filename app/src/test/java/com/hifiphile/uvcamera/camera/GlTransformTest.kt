// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test

class GlTransformTest {
    @Test
    fun identityCoversTheBuffer() {
        assertArrayEquals(
            floatArrayOf(0f, 0f, 1f, 0f, 0f, 1f, 1f, 1f),
            transformTexCoords(1920, 1080, 1920, 1080, Transform()),
            0f,
        )
    }

    /**
     * Sampling at each output pixel's center must hit the source pixel the
     * CPU transform copies, for every rotation and mirroring.
     */
    @Test
    fun matchesYuvTransform() {
        val (sw, sh, ow, oh) = listOf(8, 6, 6, 4)
        // Luma values are the source pixel index; chroma doesn't matter.
        val y = Plane(ByteArray(sw * sh) { it.toByte() }, sw, 1)
        val c = Plane(ByteArray(sw * sh / 4), sw / 2, 1)

        for (rotation in Rotation.entries) {
            for (mirrorH in listOf(false, true)) {
                for (mirrorV in listOf(false, true)) {
                    val t = Transform(rotation, mirrorH, mirrorV)
                    val (outW, outH) = ow to oh
                    val (rw, rh) = t.rotatedSize(sw, sh)
                    if (rw < outW || rh < outH) continue

                    val cpu = ByteArray(outW * outH * 3 / 2)
                    YuvTransform(sw, sh, outW, outH, t).toNv21(y, c, c, cpu)

                    val tc = transformTexCoords(sw, sh, outW, outH, t)
                    for (py in 0 until outH) {
                        for (px in 0 until outW) {
                            val u = (px + 0.5) / outW
                            val v = (py + 0.5) / outH
                            // Bilinear interpolation of the corner coordinates.
                            fun lerp(i: Int) = (1 - v) * ((1 - u) * tc[i] + u * tc[2 + i]) +
                                v * ((1 - u) * tc[4 + i] + u * tc[6 + i])
                            val sx = (lerp(0) * sw).toInt()
                            val sy = (lerp(1) * sh).toInt()
                            assertEquals("$t at ($px, $py)", sy * sw + sx, cpu[py * outW + px].toInt())
                        }
                    }
                }
            }
        }
    }
}

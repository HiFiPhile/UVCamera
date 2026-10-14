// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import org.junit.Assert.assertArrayEquals
import org.junit.Test

class YuvConvertTest {
    private fun bytes(vararg values: Int) = ByteArray(values.size) { values[it].toByte() }

    // A 4x2 image. Y has a row stride of 6 (padding), chroma is semi-planar
    // like most camera HALs: U and V planes share memory with pixel stride 2.
    private val y = Plane(bytes(
        1, 2, 3, 4, 0, 0,
        5, 6, 7, 8, 0, 0,
    ), rowStride = 6, pixelStride = 1)
    private val uv = bytes(10, 20, 11, 21)
    private val u = Plane(uv, rowStride = 4, pixelStride = 2)
    private val v = Plane(uv.copyOfRange(1, 4), rowStride = 4, pixelStride = 2)

    @Test
    fun nv21() {
        val out = ByteArray(4 * 2 * 3 / 2)
        yuv420ToNv21(4, 2, y, u, v, out)
        assertArrayEquals(bytes(1, 2, 3, 4, 5, 6, 7, 8, 20, 10, 21, 11), out)
    }

    @Test
    fun nv21Rotated() {
        val out = ByteArray(4 * 2 * 3 / 2)
        yuv420ToNv21(4, 2, y, u, v, out, rotate180 = true)
        // Luma reversed (rows and columns); the single chroma row reversed by pair.
        assertArrayEquals(bytes(8, 7, 6, 5, 4, 3, 2, 1, 21, 11, 20, 10), out)
    }

    @Test
    fun yuyvRotated() {
        val out = ByteArray(4 * 2 * 2)
        yuv420ToYuyv(4, 2, y, u, v, out, rotate180 = true)
        assertArrayEquals(
            bytes(8, 11, 7, 21, 6, 10, 5, 20, 4, 11, 3, 21, 2, 10, 1, 20),
            out,
        )
    }

    @Test
    fun yuyv() {
        val out = ByteArray(4 * 2 * 2)
        yuv420ToYuyv(4, 2, y, u, v, out)
        assertArrayEquals(
            bytes(1, 10, 2, 20, 3, 11, 4, 21, 5, 10, 6, 20, 7, 11, 8, 21),
            out,
        )
    }
}

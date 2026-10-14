// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

/**
 * Checks [YuvTransform] against a naive reference: build the rotated image
 * as a matrix, mirror it, crop the center. Independent of the row-walk math.
 */
class YuvTransformTest {
    // Source 12x8 with padded Y rows and semi-planar chroma, like a camera.
    private val sw = 12
    private val sh = 8
    private val yStride = 16
    private val cStride = 16

    private fun yValue(x: Int, y: Int) = (y * sw + x) and 0x7f
    private fun uValue(x: Int, y: Int) = 0x80 or ((y * 8 + x) and 0x3f)
    private fun vValue(x: Int, y: Int) = 0xc0 or ((y * 8 + x) and 0x3f)

    private val yPlane = Plane(ByteArray(yStride * sh).also { d ->
        for (y in 0 until sh) for (x in 0 until sw) d[y * yStride + x] = yValue(x, y).toByte()
    }, yStride, 1)

    // Interleaved UVUV...: U at even offsets, V plane starting one byte in.
    private val uv = ByteArray(cStride * sh / 2).also { d ->
        for (y in 0 until sh / 2) for (x in 0 until sw / 2) {
            d[y * cStride + 2 * x] = uValue(x, y).toByte()
            d[y * cStride + 2 * x + 1] = vValue(x, y).toByte()
        }
    }
    private val uPlane = Plane(uv, cStride, 2)
    private val vPlane = Plane(uv.copyOfRange(1, uv.size), cStride, 2)

    /** Reference: transform a w x h matrix given by [value]. */
    private fun reference(
        w: Int, h: Int, t: Transform, ow: Int, oh: Int, cropX: Int, cropY: Int,
        value: (Int, Int) -> Int,
    ): Array<IntArray> {
        // Rotate clockwise by building the rotated matrix explicitly.
        var m = Array(h) { y -> IntArray(w) { x -> value(x, y) } }
        repeat(t.rotation.degrees / 90) {
            val rh = m[0].size
            val rw = m.size
            m = Array(rh) { y -> IntArray(rw) { x -> m[rw - 1 - x][y] } }
        }
        if (t.mirrorH) m = Array(m.size) { y -> m[y].reversedArray() }
        if (t.mirrorV) m = m.reversedArray()
        return Array(oh) { y -> IntArray(ow) { x -> m[y + cropY][x + cropX] } }
    }

    private fun check(t: Transform, ow: Int, oh: Int) {
        val (rw, rh) = t.rotatedSize(sw, sh)
        val cx = (rw - ow) / 2 and 1.inv()
        val cy = (rh - oh) / 2 and 1.inv()
        val ry = reference(sw, sh, t, ow, oh, cx, cy, ::yValue)
        val ru = reference(sw / 2, sh / 2, t, ow / 2, oh / 2, cx / 2, cy / 2, ::uValue)
        val rv = reference(sw / 2, sh / 2, t, ow / 2, oh / 2, cx / 2, cy / 2, ::vValue)

        val xf = YuvTransform(sw, sh, ow, oh, t)

        val nv21 = ByteArray(ow * oh * 3 / 2)
        xf.toNv21(yPlane, uPlane, vPlane, nv21)
        val expectedNv21 = ByteArray(nv21.size).also { e ->
            var o = 0
            for (y in 0 until oh) for (x in 0 until ow) e[o++] = ry[y][x].toByte()
            for (y in 0 until oh / 2) for (x in 0 until ow / 2) {
                e[o++] = rv[y][x].toByte()
                e[o++] = ru[y][x].toByte()
            }
        }
        assertArrayEquals("NV21 $t ${ow}x$oh", expectedNv21, nv21)

        val yuyv = ByteArray(ow * oh * 2)
        xf.toYuyv(yPlane, uPlane, vPlane, yuyv)
        val expectedYuyv = ByteArray(yuyv.size).also { e ->
            var o = 0
            for (y in 0 until oh) for (p in 0 until ow / 2) {
                e[o++] = ry[y][2 * p].toByte()
                e[o++] = ru[y / 2][p].toByte()
                e[o++] = ry[y][2 * p + 1].toByte()
                e[o++] = rv[y / 2][p].toByte()
            }
        }
        assertArrayEquals("YUYV $t ${ow}x$oh", expectedYuyv, yuyv)
    }

    @Test
    fun everyRotationAndMirrorWithoutCrop() {
        for (rotation in Rotation.entries) for (h in listOf(false, true)) for (v in listOf(false, true)) {
            val t = Transform(rotation, h, v)
            val (rw, rh) = t.rotatedSize(sw, sh)
            check(t, rw, rh)
        }
    }

    @Test
    fun everyRotationAndMirrorWithCenterCrop() {
        // 8x6 is a landscape crop of both the 12x8 source and its 8x12 rotation.
        for (rotation in Rotation.entries) for (h in listOf(false, true)) for (v in listOf(false, true)) {
            check(Transform(rotation, h, v), 8, 6)
        }
    }

    @Test
    fun sourceTooSmallIsRejected() {
        assertThrows(IllegalArgumentException::class.java) {
            YuvTransform(sw, sh, 10, 6, Transform(Rotation.R90))
        }
    }

    @Test
    fun rotationArithmetic() {
        assertEquals(Rotation.R90, Rotation.R180 + Rotation.R270)
        assertEquals(Rotation.R270, Rotation.fromDegrees(-90))
        assertEquals(true, Transform().isIdentity)
        assertEquals(false, Transform(mirrorH = true).isIdentity)
    }
}

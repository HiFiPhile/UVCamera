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

    /** Reference: transform a w x h matrix given by [value]; [black] outside it. */
    private fun reference(
        w: Int, h: Int, t: Transform, ow: Int, oh: Int, cropX: Int, cropY: Int,
        black: Int,
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
        return Array(oh) { y ->
            IntArray(ow) { x -> m.getOrNull(y + cropY)?.getOrNull(x + cropX) ?: black }
        }
    }

    private fun check(t: Transform, ow: Int, oh: Int) {
        val (rw, rh) = t.rotatedSize(sw, sh)
        val cx = Math.floorDiv(rw - ow, 2) and 1.inv()
        val cy = Math.floorDiv(rh - oh, 2) and 1.inv()
        val ry = reference(sw, sh, t, ow, oh, cx, cy, 0, ::yValue)
        val ru = reference(sw / 2, sh / 2, t, ow / 2, oh / 2, cx / 2, cy / 2, 128, ::uValue)
        val rv = reference(sw / 2, sh / 2, t, ow / 2, oh / 2, cx / 2, cy / 2, 128, ::vValue)

        val xf = YuvTransform(sw, sh, ow, oh, t)

        // Apply the walks as JpegEncoder's native code does, into I420.
        val (yw, cw) = xf.walks(yPlane, uPlane)
        fun sample(p: Plane, w: YuvTransform.RowWalk, x: Int, y: Int, black: Int): Byte {
            val i = x - w.firsts[y]
            return if (i in 0 until w.counts[y]) p.data[w.starts[y] + i * w.steps[y]] else black.toByte()
        }
        val i420 = ByteArray(ow * oh * 3 / 2)
        var o = 0
        for (y in 0 until oh) for (x in 0 until ow) i420[o++] = sample(yPlane, yw, x, y, 0)
        for (plane in listOf(uPlane, vPlane)) {
            for (y in 0 until oh / 2) for (x in 0 until ow / 2) i420[o++] = sample(plane, cw, x, y, 128)
        }
        val expected = ByteArray(i420.size).also { e ->
            var i = 0
            for (y in 0 until oh) for (x in 0 until ow) e[i++] = ry[y][x].toByte()
            for (r in listOf(ru, rv)) for (y in 0 until oh / 2) for (x in 0 until ow / 2) e[i++] = r[y][x].toByte()
        }
        assertArrayEquals("I420 $t ${ow}x$oh", expected, i420)
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
    fun everyRotationAndMirrorWithBlackAround() {
        // 10x6 is wider than the 8x12 rotation; 16x10 is larger both ways
        // than the source; 14x6 pads one way and crops the other.
        for (rotation in Rotation.entries) for (h in listOf(false, true)) for (v in listOf(false, true)) {
            for ((ow, oh) in listOf(10 to 6, 16 to 10, 14 to 6)) check(Transform(rotation, h, v), ow, oh)
        }
    }

    @Test
    fun oddSizesAreRejected() {
        assertThrows(IllegalArgumentException::class.java) {
            YuvTransform(sw, sh, 9, 6, Transform())
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

// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import android.media.Image
import java.nio.ByteBuffer

/**
 * One plane of a YUV_420_888 image, copied out of its ByteBuffer so the
 * conversions below can be plain array loops (and unit tested).
 */
class Plane(val data: ByteArray, val rowStride: Int, val pixelStride: Int) {
    operator fun get(x: Int, y: Int): Byte = data[y * rowStride + x * pixelStride]

    companion object {
        fun from(plane: Image.Plane, scratch: ByteArray?): Plane {
            val buffer: ByteBuffer = plane.buffer
            val size = buffer.remaining()
            val data = if (scratch != null && scratch.size == size) scratch else ByteArray(size)
            buffer.get(data)
            return Plane(data, plane.rowStride, plane.pixelStride)
        }
    }
}

/** Convert YUV 4:2:0 planes to NV21 (Y plane, then interleaved V/U), as YuvImage expects. */
fun yuv420ToNv21(width: Int, height: Int, y: Plane, u: Plane, v: Plane, out: ByteArray) {
    require(out.size >= width * height * 3 / 2) { "Output too small" }

    var o = 0
    for (row in 0 until height) {
        System.arraycopy(y.data, row * y.rowStride, out, o, width)
        o += width
    }
    for (row in 0 until height / 2) {
        for (col in 0 until width / 2) {
            out[o++] = v[col, row]
            out[o++] = u[col, row]
        }
    }
}

/** Convert YUV 4:2:0 planes to packed YUYV 4:2:2, repeating each chroma row twice. */
fun yuv420ToYuyv(width: Int, height: Int, y: Plane, u: Plane, v: Plane, out: ByteArray) {
    require(out.size >= width * height * 2) { "Output too small" }
    require(width % 2 == 0) { "Width must be even" }

    var o = 0
    for (row in 0 until height) {
        val yRow = row * y.rowStride
        val cRow = row / 2
        for (col in 0 until width / 2) {
            out[o++] = y.data[yRow + col * 2 * y.pixelStride]
            out[o++] = u[col, cRow]
            out[o++] = y.data[yRow + (col * 2 + 1) * y.pixelStride]
            out[o++] = v[col, cRow]
        }
    }
}

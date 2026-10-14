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

/**
 * Convert YUV 4:2:0 planes to NV21 (Y plane, then interleaved V/U), as
 * YuvImage expects, optionally rotating the image by 180°.
 */
fun yuv420ToNv21(
    width: Int,
    height: Int,
    y: Plane,
    u: Plane,
    v: Plane,
    out: ByteArray,
    rotate180: Boolean = false,
) {
    require(out.size >= width * height * 3 / 2) { "Output too small" }

    var o = 0
    if (!rotate180) {
        for (row in 0 until height) {
            System.arraycopy(y.data, row * y.rowStride, out, o, width)
            o += width
        }
    } else {
        for (row in height - 1 downTo 0) {
            val base = row * y.rowStride
            for (col in width - 1 downTo 0) {
                out[o++] = y.data[base + col * y.pixelStride]
            }
        }
    }

    val cw = width / 2
    val ch = height / 2
    for (r in 0 until ch) {
        val row = if (rotate180) ch - 1 - r else r
        for (c in 0 until cw) {
            val col = if (rotate180) cw - 1 - c else c
            out[o++] = v[col, row]
            out[o++] = u[col, row]
        }
    }
}

/**
 * Convert YUV 4:2:0 planes to packed YUYV 4:2:2, repeating each chroma row
 * twice, optionally rotating the image by 180°.
 */
fun yuv420ToYuyv(
    width: Int,
    height: Int,
    y: Plane,
    u: Plane,
    v: Plane,
    out: ByteArray,
    rotate180: Boolean = false,
) {
    require(out.size >= width * height * 2) { "Output too small" }
    require(width % 2 == 0) { "Width must be even" }

    val pairs = width / 2
    var o = 0
    for (r in 0 until height) {
        val row = if (rotate180) height - 1 - r else r
        val yRow = row * y.rowStride
        val cRow = row / 2
        for (p in 0 until pairs) {
            // Output pair p comes from input pair (pairs - 1 - p) when
            // rotated, with its two luma samples swapped.
            val inPair = if (rotate180) pairs - 1 - p else p
            val first = inPair * 2
            val (y0, y1) = if (rotate180) first + 1 to first else first to first + 1
            out[o++] = y.data[yRow + y0 * y.pixelStride]
            out[o++] = u[inPair, cRow]
            out[o++] = y.data[yRow + y1 * y.pixelStride]
            out[o++] = v[inPair, cRow]
        }
    }
}

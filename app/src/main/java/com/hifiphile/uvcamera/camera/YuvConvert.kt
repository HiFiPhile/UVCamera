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

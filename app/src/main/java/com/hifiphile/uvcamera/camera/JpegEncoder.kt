// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

/**
 * Software JPEG with libjpeg-turbo (4:2:0), transforming the camera planes
 * on the way in native code. Each calling thread keeps its own compressor.
 */
object JpegEncoder {
    init {
        System.loadLibrary("uvcamera")
    }

    /** Apply [xf] to the camera planes and compress the result at [quality]. */
    fun encode(xf: YuvTransform, y: Plane, u: Plane, v: Plane, quality: Int): ByteArray {
        val (yw, cw) = xf.walks(y, u)
        return nativeEncode(xf.outWidth, xf.outHeight, quality, y.data, u.data, v.data,
            yw.starts, yw.steps, cw.starts, cw.steps)
    }

    @JvmStatic
    private external fun nativeEncode(
        width: Int,
        height: Int,
        quality: Int,
        y: ByteArray,
        u: ByteArray,
        v: ByteArray,
        yStarts: IntArray,
        ySteps: IntArray,
        cStarts: IntArray,
        cSteps: IntArray,
    ): ByteArray
}

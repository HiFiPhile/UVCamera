// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

/** Clockwise rotation. */
enum class Rotation(val degrees: Int) {
    R0(0),
    R90(90),
    R180(180),
    R270(270);

    /** Swaps width and height. */
    val isQuarterTurn: Boolean get() = this == R90 || this == R270

    operator fun plus(other: Rotation): Rotation = fromDegrees(degrees + other.degrees)

    companion object {
        fun fromDegrees(degrees: Int): Rotation {
            val d = ((degrees % 360) + 360) % 360
            return entries.find { it.degrees == d }
                ?: throw IllegalArgumentException("Not a multiple of 90°: $degrees")
        }
    }
}

/**
 * How the camera image becomes the host's image: rotate clockwise, then
 * mirror (in the rotated image: [mirrorH] flips left/right, [mirrorV]
 * top/bottom), then crop the center to the output size.
 */
data class Transform(
    val rotation: Rotation = Rotation.R0,
    val mirrorH: Boolean = false,
    val mirrorV: Boolean = false,
) {
    val isIdentity: Boolean get() = rotation == Rotation.R0 && !mirrorH && !mirrorV

    /** Size of the camera image after rotating it. */
    fun rotatedSize(width: Int, height: Int): Pair<Int, Int> =
        if (rotation.isQuarterTurn) height to width else width to height
}

/**
 * Maps output pixels to source pixels for one source size, output size and
 * [Transform]. The rotated source is centered on the output: cropped where
 * it is larger, and surrounded by black where it is smaller.
 *
 * Each output row is a straight line through the source (a row, a column,
 * or either reversed), so the inner loops only add a fixed step to an index.
 */
class YuvTransform(
    private val srcWidth: Int,
    private val srcHeight: Int,
    val outWidth: Int,
    val outHeight: Int,
    val transform: Transform,
) {
    // Offsets of the output in the rotated image, even so chroma stays
    // aligned: positive crops, negative pads.
    private val cropX: Int
    private val cropY: Int

    init {
        require(outWidth % 2 == 0 && outHeight % 2 == 0) { "Output size must be even" }
        require(srcWidth % 2 == 0 && srcHeight % 2 == 0) { "Source size must be even" }
        val (rw, rh) = transform.rotatedSize(srcWidth, srcHeight)
        cropX = Math.floorDiv(rw - outWidth, 2) and 1.inv()
        cropY = Math.floorDiv(rh - outHeight, 2) and 1.inv()
    }

    /**
     * Source coordinates (packed x, y) of output pixel (x, y) in a plane of
     * size [w]x[h], with crop offsets [cx], [cy] in the rotated plane.
     */
    private fun source(x: Int, y: Int, w: Int, h: Int, cx: Int, cy: Int): Long {
        // Undo the crop, then the mirror (over the whole rotated image), then
        // the rotation. Mirroring after cropping would differ by a row or
        // column when the crop can't be exactly centered.
        val (rw, rh) = transform.rotatedSize(w, h)
        val xc = x + cx
        val yc = y + cy
        val xr = if (transform.mirrorH) rw - 1 - xc else xc
        val yr = if (transform.mirrorV) rh - 1 - yc else yc
        val (sx, sy) = when (transform.rotation) {
            Rotation.R0 -> xr to yr
            Rotation.R90 -> yr to h - 1 - xr
            Rotation.R180 -> w - 1 - xr to h - 1 - yr
            Rotation.R270 -> w - 1 - yr to xr
        }
        return (sx.toLong() shl 32) or (sy.toLong() and 0xffffffffL)
    }

    /**
     * For each output row of a plane: the output columns that show the
     * source, [firsts] to [firsts] + [counts] (the rest is black), the index
     * of the first one's sample, and the index step per output pixel, given
     * the plane's strides.
     */
    class RowWalk(val starts: IntArray, val steps: IntArray, val firsts: IntArray, val counts: IntArray)

    private fun walk(w: Int, h: Int, ow: Int, oh: Int, cx: Int, cy: Int, rowStride: Int, pixelStride: Int): RowWalk {
        val (rw, rh) = transform.rotatedSize(w, h)
        val starts = IntArray(oh)
        val steps = IntArray(oh)
        val firsts = IntArray(oh)
        val counts = IntArray(oh)
        // Columns of the output inside the rotated image.
        val x0 = maxOf(0, -cx)
        val x1 = minOf(ow, rw - cx)
        for (y in 0 until oh) {
            if (y + cy !in 0 until rh || x1 <= x0) {
                continue
            }
            val a = source(x0, y, w, h, cx, cy)
            val b = source(if (x1 - x0 > 1) x0 + 1 else x0, y, w, h, cx, cy)
            val ax = (a shr 32).toInt()
            val ay = a.toInt()
            val bx = (b shr 32).toInt()
            val by = b.toInt()
            starts[y] = ay * rowStride + ax * pixelStride
            steps[y] = (by - ay) * rowStride + (bx - ax) * pixelStride
            firsts[y] = x0
            counts[y] = x1 - x0
        }
        return RowWalk(starts, steps, firsts, counts)
    }

    // Walks are cached per plane layout; strides are fixed for a camera stream.
    private var yWalk: RowWalk? = null
    private var yKey = 0L
    private var cWalk: RowWalk? = null
    private var cKey = 0L

    // Shared by the encoder's worker threads.
    @Synchronized
    private fun lumaWalk(p: Plane): RowWalk {
        val key = (p.rowStride.toLong() shl 32) or p.pixelStride.toLong()
        if (yWalk == null || yKey != key) {
            yWalk = walk(srcWidth, srcHeight, outWidth, outHeight, cropX, cropY, p.rowStride, p.pixelStride)
            yKey = key
        }
        return yWalk!!
    }

    @Synchronized
    private fun chromaWalk(p: Plane): RowWalk {
        val key = (p.rowStride.toLong() shl 32) or p.pixelStride.toLong()
        if (cWalk == null || cKey != key) {
            cWalk = walk(srcWidth / 2, srcHeight / 2, outWidth / 2, outHeight / 2,
                cropX / 2, cropY / 2, p.rowStride, p.pixelStride)
            cKey = key
        }
        return cWalk!!
    }

    /**
     * Walks for the output's luma rows over [y] and chroma rows over [u]
     * (U and V planes share strides in YUV_420_888).
     */
    fun walks(y: Plane, u: Plane): Pair<RowWalk, RowWalk> = lumaWalk(y) to chromaWalk(u)
}

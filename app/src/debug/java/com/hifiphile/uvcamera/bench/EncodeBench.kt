// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.bench

import android.graphics.BitmapFactory
import android.graphics.Color
import android.graphics.ImageFormat
import android.graphics.Rect
import android.graphics.YuvImage
import com.hifiphile.uvcamera.camera.JpegEncoder
import com.hifiphile.uvcamera.camera.Plane
import com.hifiphile.uvcamera.camera.Rotation
import com.hifiphile.uvcamera.camera.Transform
import com.hifiphile.uvcamera.camera.YuvTransform
import java.io.ByteArrayOutputStream
import kotlin.random.Random

/**
 * Times software JPEG at 1080p on synthetic camera frames: [JpegEncoder]
 * per rotation, and Android's YuvImage for comparison. Also checks that the
 * native transform matches [YuvTransform]'s walks: the decoded Y, Cb and
 * Cr must be close to the walked source planes. Debug builds only:
 *
 *     A=$(pm path com.hifiphile.uvcamera | cut -d: -f2)
 *     CLASSPATH=$A app_process -Djava.library.path=/system/lib64:$A!/lib/arm64-v8a \
 *         / com.hifiphile.uvcamera.bench.EncodeBench
 */
object EncodeBench {
    private const val W = 1920
    private const val H = 1080
    private const val N = 60
    private const val QUALITY = 85

    /** Semi-planar YUV_420_888 planes with gradients and sensor-like noise. */
    private fun planes(w: Int, h: Int): Triple<Plane, Plane, Plane> {
        val rnd = Random(1)
        val y = ByteArray(w * h) { i ->
            ((i % w * 200 / w + i / w * 40 / h) + rnd.nextInt(-6, 7)).coerceIn(0, 255).toByte()
        }
        // Interleaved UV rows, as a camera's chroma planes share one buffer.
        val uv = ByteArray(w * h / 2) { i -> (128 + (i % w) / 20 + rnd.nextInt(-3, 4)).toByte() }
        return Triple(Plane(y, w, 1), Plane(uv, w, 2), Plane(uv.copyOfRange(1, uv.size), w, 2))
    }

    private inline fun time(label: String, block: () -> Int) {
        repeat(10) { block() }
        val start = System.nanoTime()
        var size = 0
        repeat(N) { size = block() }
        println("%-34s %6.2f ms, %d KiB".format(label, (System.nanoTime() - start) / 1e6 / N, size / 1024))
    }

    /**
     * Mild gradients that don't clip in RGB: luma across, U across and V
     * down, each different under every rotation and mirror.
     */
    private fun checkPlanes(w: Int, h: Int): Triple<Plane, Plane, Plane> {
        val y = ByteArray(w * h) { i -> (60 + i % w * 100 / w + i / w * 30 / h).toByte() }
        val uv = ByteArray(w * h / 2) { i ->
            val x = i % w / 2
            val row = i / w
            (if (i % 2 == 0) 108 + x * 40 / (w / 2) else 108 + row * 40 / (h / 2)).toByte()
        }
        return Triple(Plane(y, w, 1), Plane(uv, w, 2), Plane(uv.copyOfRange(1, uv.size), w, 2))
    }

    /** Mean absolute Y, Cb and Cr differences between the JPEG and the walked planes. */
    private fun errors(xf: YuvTransform, y: Plane, u: Plane, v: Plane, jpeg: ByteArray): DoubleArray {
        val bitmap = BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size)
        val (yw, cw) = xf.walks(y, u)
        val sums = DoubleArray(3)
        fun at(p: Plane, walk: YuvTransform.RowWalk, x: Int, row: Int) =
            p.data[walk.starts[row] + x * walk.steps[row]].toInt() and 0xff
        for (row in 0 until H) for (x in 0 until W) {
            val c = bitmap.getPixel(x, row)
            val (r, g, b) = Triple(Color.red(c), Color.green(c), Color.blue(c))
            sums[0] += kotlin.math.abs(0.299 * r + 0.587 * g + 0.114 * b - at(y, yw, x, row))
            sums[1] += kotlin.math.abs(128 - 0.168736 * r - 0.331264 * g + 0.5 * b - at(u, cw, x / 2, row / 2))
            sums[2] += kotlin.math.abs(128 + 0.5 * r - 0.418688 * g - 0.081312 * b - at(v, cw, x / 2, row / 2))
        }
        return DoubleArray(3) { sums[it] / (W * H) }
    }

    @JvmStatic
    fun main(args: Array<String>) {
        System.loadLibrary("uvcamera")
        for ((r, sw, sh) in listOf(
            Triple(Rotation.R0, W, H),
            Triple(Rotation.R180, W, H),
            Triple(Rotation.R90, 2560, 1920),
        )) {
            val (y, u, v) = planes(sw, sh)
            val xf = YuvTransform(sw, sh, W, H, Transform(r))
            time("JpegEncoder ${r.degrees}° from ${sw}x$sh") { JpegEncoder.encode(xf, y, u, v, QUALITY).size }
            val (cy, cu, cv) = checkPlanes(sw, sh)
            val e = errors(xf, cy, cu, cv, JpegEncoder.encode(xf, cy, cu, cv, QUALITY))
            println("  mean error Y %.2f, Cb %.2f, Cr %.2f".format(e[0], e[1], e[2]))
        }

        // The previous path: NV21 (Y, then interleaved VU) into YuvImage.
        val (y, uv, _) = planes(W, H)
        val nv21 = y.data + ByteArray(W * H / 2) { i -> uv.data[i xor 1] }
        val out = ByteArrayOutputStream(W * H / 4)
        val rect = Rect(0, 0, W, H)
        time("YuvImage (compression only)") {
            out.reset()
            YuvImage(nv21, ImageFormat.NV21, W, H, null).compressToJpeg(rect, QUALITY, out)
            out.size()
        }
    }
}

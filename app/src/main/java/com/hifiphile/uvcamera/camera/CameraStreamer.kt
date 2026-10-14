// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import android.annotation.SuppressLint
import android.graphics.ImageFormat
import android.graphics.Rect
import android.graphics.YuvImage
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.SessionConfiguration
import android.media.ImageReader
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import android.util.Log
import android.util.Range
import android.util.Size
import com.hifiphile.uvcamera.daemon.FormatKind
import com.hifiphile.uvcamera.daemon.StreamParams
import java.io.ByteArrayOutputStream
import java.util.concurrent.Executor

/** How frames for a stream are produced. */
enum class EncodeMode {
    /** The camera's hardware JPEG encoder: no CPU work per frame. */
    HW_JPEG,

    /** YUV from the camera, compressed with YuvImage (libjpeg-turbo). */
    YUV_JPEG,

    /** YUV from the camera, repacked to YUYV. */
    YUYV,
}

/** Receives encoded frames: [length] bytes of [data], captured at [ptsUs]. */
fun interface FrameSink {
    fun onFrame(data: ByteArray, length: Int, ptsUs: Long)
}

/**
 * Runs one camera at a time for the host's current stream. All camera work
 * happens on a dedicated handler thread; [start] and [stop] may be called
 * from any thread.
 */
class CameraStreamer(private val manager: CameraManager, private val cameraId: String) {
    companion object {
        private const val TAG = "CameraStreamer"
        private const val JPEG_QUALITY = 85
        private const val MAX_IMAGES = 3

        /**
         * Pick how to produce frames in [params] with this camera. The camera's
         * JPEG encoder can't rotate, so a rotated MJPEG stream goes through YUV.
         */
        fun chooseMode(chars: CameraCharacteristics, params: StreamParams, rotate180: Boolean): EncodeMode {
            if (params.kind == FormatKind.YUYV) {
                return EncodeMode.YUYV
            }
            if (rotate180) {
                return EncodeMode.YUV_JPEG
            }

            val map = chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
            val size = Size(params.width, params.height)
            val hwJpeg = map?.getOutputSizes(ImageFormat.JPEG)?.contains(size) == true &&
                map.getOutputMinFrameDuration(ImageFormat.JPEG, size)
                    .let { it in 1..params.interval * 100L }
            return if (hwJpeg) EncodeMode.HW_JPEG else EncodeMode.YUV_JPEG
        }

        /**
         * The AE target range for [fps]: exactly [fps, fps] if offered, else
         * the narrowest range topping out at [fps], else any containing it.
         */
        fun chooseFpsRange(ranges: List<Range<Int>>, fps: Int): Range<Int>? =
            ranges.find { it.lower == fps && it.upper == fps }
                ?: ranges.filter { it.upper == fps }.maxByOrNull { it.lower }
                ?: ranges.filter { fps in it }.minByOrNull { it.upper - it.lower }
    }

    private val thread = HandlerThread("camera").apply { start() }
    private val handler = Handler(thread.looper)
    private val executor = Executor { handler.post(it) }

    private var device: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var reader: ImageReader? = null
    private var active: Pair<StreamParams, Boolean>? = null

    /** Start (or restart) producing frames in [params], rotated by 180° if [rotate180]. */
    fun start(params: StreamParams, rotate180: Boolean, sink: FrameSink, onError: (String) -> Unit) {
        handler.post {
            if (active == params to rotate180) {
                return@post
            }
            closeCamera()
            try {
                openCamera(params, rotate180, sink, onError)
            } catch (e: Exception) {
                Log.e(TAG, "Failed to start camera", e)
                closeCamera()
                onError(e.toString())
            }
        }
    }

    fun stop() {
        handler.post { closeCamera() }
    }

    /** Stop and release the handler thread. */
    fun release() {
        handler.post {
            closeCamera()
            thread.quitSafely()
        }
    }

    @SuppressLint("MissingPermission") // Checked before the service starts.
    private fun openCamera(
        params: StreamParams,
        rotate180: Boolean,
        sink: FrameSink,
        onError: (String) -> Unit,
    ) {
        val chars = manager.getCameraCharacteristics(cameraId)
        val mode = chooseMode(chars, params, rotate180)
        val fps = Math.round(params.fps).toInt()
        Log.i(TAG, "Starting camera $cameraId: $params via $mode, rotate180=$rotate180")

        val format = if (mode == EncodeMode.HW_JPEG) ImageFormat.JPEG else ImageFormat.YUV_420_888
        val reader = ImageReader.newInstance(params.width, params.height, format, MAX_IMAGES)
        val realtimeTimestamps = chars.get(CameraCharacteristics.SENSOR_INFO_TIMESTAMP_SOURCE) ==
            CameraCharacteristics.SENSOR_INFO_TIMESTAMP_SOURCE_REALTIME
        reader.setOnImageAvailableListener(
            FrameEncoder(mode, params, rotate180, realtimeTimestamps, sink), handler)
        this.reader = reader
        active = params to rotate180

        manager.openCamera(cameraId, executor, object : CameraDevice.StateCallback() {
            override fun onOpened(camera: CameraDevice) {
                if (this@CameraStreamer.reader !== reader) {
                    // Stopped or restarted while opening.
                    camera.close()
                    return
                }
                device = camera
                createSession(camera, reader, chars, mode, fps, onError)
            }

            override fun onDisconnected(camera: CameraDevice) {
                Log.w(TAG, "Camera $cameraId disconnected (taken by another app?)")
                camera.close()
                if (device === camera) {
                    closeCamera()
                    onError("Camera disconnected")
                }
            }

            override fun onError(camera: CameraDevice, error: Int) {
                Log.e(TAG, "Camera $cameraId error $error")
                camera.close()
                if (device === camera) {
                    closeCamera()
                    onError("Camera error $error")
                }
            }
        })
    }

    private fun createSession(
        camera: CameraDevice,
        reader: ImageReader,
        chars: CameraCharacteristics,
        mode: EncodeMode,
        fps: Int,
        onError: (String) -> Unit,
    ) {
        val outputs = listOf(OutputConfiguration(reader.surface))
        val config = SessionConfiguration(
            SessionConfiguration.SESSION_REGULAR,
            outputs,
            executor,
            object : CameraCaptureSession.StateCallback() {
                override fun onConfigured(s: CameraCaptureSession) {
                    if (device !== camera) {
                        s.close()
                        return
                    }
                    session = s

                    // PREVIEW rather than RECORD: it is tuned for latency, and
                    // video stabilization would buffer several frames.
                    val request = camera.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW).apply {
                        addTarget(reader.surface)
                        set(CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO)
                        set(
                            CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE,
                            CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE_OFF,
                        )
                        set(
                            CaptureRequest.CONTROL_AF_MODE,
                            CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO,
                        )
                        chars.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES)
                            ?.let { chooseFpsRange(it.toList(), fps) }
                            ?.let { set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, it) }
                        if (mode == EncodeMode.HW_JPEG) {
                            set(CaptureRequest.JPEG_QUALITY, JPEG_QUALITY.toByte())
                            set(CaptureRequest.JPEG_ORIENTATION, 0)
                        }
                    }.build()

                    s.setSingleRepeatingRequest(request, executor, object :
                        CameraCaptureSession.CaptureCallback() {})
                    Log.i(TAG, "Camera $cameraId streaming, AE range " +
                        "${request.get(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE)}")
                }

                override fun onConfigureFailed(s: CameraCaptureSession) {
                    Log.e(TAG, "Failed to configure capture session")
                    s.close()
                    if (device === camera) {
                        closeCamera()
                        onError("Failed to configure camera session")
                    }
                }
            },
        )
        camera.createCaptureSession(config)
    }

    private fun closeCamera() {
        session?.close()
        session = null
        device?.close()
        device = null
        reader?.close()
        reader = null
        if (active != null) {
            Log.i(TAG, "Camera $cameraId closed")
        }
        active = null
    }
}

/** Turns each camera image into a frame for the host, on the camera thread. */
private class FrameEncoder(
    private val mode: EncodeMode,
    params: StreamParams,
    private val rotate180: Boolean,
    /** Image timestamps are in the elapsedRealtime (boot time) clock. */
    private val realtimeTimestamps: Boolean,
    private val sink: FrameSink,
) : ImageReader.OnImageAvailableListener {
    private val width = params.width
    private val height = params.height

    // Reused between frames.
    private val jpegOut = ByteArrayOutputStream(width * height / 4)
    private var yScratch: ByteArray? = null
    private var uScratch: ByteArray? = null
    private var vScratch: ByteArray? = null
    private val nv21 = if (mode == EncodeMode.YUV_JPEG) ByteArray(width * height * 3 / 2) else null
    private val rect = Rect(0, 0, width, height)

    // Encode cost, logged every STATS_PERIOD_NS.
    private var statsSince = System.nanoTime()
    private var statsFrames = 0
    private var statsTotalNs = 0L
    private var statsMaxNs = 0L

    override fun onImageAvailable(reader: ImageReader) {
        val image = runCatching { reader.acquireLatestImage() }.getOrNull() ?: return
        // Capture time in CLOCK_MONOTONIC (System.nanoTime), as the protocol
        // expects, so the daemon can measure capture-to-USB latency. The
        // sensor timestamp is the start of exposure; when it's in the boot
        // time clock, shift it by how long ago it was.
        val now = System.nanoTime()
        val ptsUs = if (realtimeTimestamps) {
            (now - (SystemClock.elapsedRealtimeNanos() - image.timestamp)) / 1000
        } else {
            now / 1000
        }

        val start = System.nanoTime()
        image.use {
            when (mode) {
                EncodeMode.HW_JPEG -> {
                    val buffer = image.planes[0].buffer
                    val length = buffer.remaining()
                    // A fresh array per frame: the sender may still hold the
                    // previous one.
                    val data = ByteArray(length)
                    buffer.get(data)
                    sink.onFrame(data, length, ptsUs)
                }

                EncodeMode.YUV_JPEG, EncodeMode.YUYV -> {
                    val planes = image.planes
                    val y = Plane.from(planes[0], yScratch).also { yScratch = it.data }
                    val u = Plane.from(planes[1], uScratch).also { uScratch = it.data }
                    val v = Plane.from(planes[2], vScratch).also { vScratch = it.data }

                    if (mode == EncodeMode.YUYV) {
                        val out = ByteArray(width * height * 2)
                        yuv420ToYuyv(width, height, y, u, v, out, rotate180)
                        sink.onFrame(out, out.size, ptsUs)
                    } else {
                        yuv420ToNv21(width, height, y, u, v, nv21!!, rotate180)
                        jpegOut.reset()
                        YuvImage(nv21, ImageFormat.NV21, width, height, null)
                            .compressToJpeg(rect, JPEG_QUALITY, jpegOut)
                        sink.onFrame(jpegOut.toByteArray(), jpegOut.size(), ptsUs)
                    }
                }
            }
        }
        recordStats(System.nanoTime() - start)
    }

    private fun recordStats(elapsedNs: Long) {
        statsFrames++
        statsTotalNs += elapsedNs
        statsMaxNs = maxOf(statsMaxNs, elapsedNs)

        val now = System.nanoTime()
        if (now - statsSince >= STATS_PERIOD_NS) {
            val seconds = (now - statsSince) / 1e9
            Log.i(TAG, "Encode $mode ${width}x$height: " +
                "%.1f fps, avg %.1f ms, max %.1f ms".format(
                    statsFrames / seconds,
                    statsTotalNs / 1e6 / statsFrames,
                    statsMaxNs / 1e6,
                ))
            statsSince = now
            statsFrames = 0
            statsTotalNs = 0
            statsMaxNs = 0
        }
    }

    companion object {
        private const val TAG = "FrameEncoder"
        private const val JPEG_QUALITY = 85
        private const val STATS_PERIOD_NS = 5_000_000_000L
    }
}

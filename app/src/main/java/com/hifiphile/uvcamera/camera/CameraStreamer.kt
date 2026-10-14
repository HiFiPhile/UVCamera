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
import android.media.Image
import android.media.ImageReader
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import android.util.Log
import android.util.Range
import android.util.Size
import com.hifiphile.uvcamera.daemon.BRIGHTNESS_DEFAULT
import com.hifiphile.uvcamera.daemon.FormatKind
import com.hifiphile.uvcamera.daemon.StreamParams
import java.io.ByteArrayOutputStream
import java.io.Closeable
import java.util.ArrayDeque
import java.util.concurrent.Executor
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.atomic.AtomicLong

/** How frames for a stream are produced. */
enum class EncodeMode {
    /** The camera's hardware JPEG encoder: no CPU work per frame. */
    HW_JPEG,

    /** YUV from the camera, transformed, compressed with YuvImage (libjpeg-turbo). */
    YUV_JPEG,

    /** YUV from the camera, transformed and repacked to YUYV. */
    YUYV,
}

/** Encoder and camera capture size for a stream. */
data class CapturePlan(val mode: EncodeMode, val width: Int, val height: Int)

/** Receives encoded frames: [length] bytes of [data], captured at [ptsUs]. */
fun interface FrameSink {
    fun onFrame(data: ByteArray, length: Int, ptsUs: Long)
}

/**
 * Runs one camera at a time for the host's current stream. All camera work
 * happens on a dedicated handler thread; the public methods may be called
 * from any thread.
 */
class CameraStreamer(private val manager: CameraManager, private val cameraId: String) {
    companion object {
        private const val TAG = "CameraStreamer"
        private const val JPEG_QUALITY = 85
        private const val MAX_IMAGES = 3

        /**
         * How to produce [params] with [transform]. The camera's JPEG encoder
         * can't rotate or mirror, so any transform goes through YUV, captured
         * large enough that the transformed image covers the output.
         *
         * @throws IllegalArgumentException if no capture size works
         */
        fun plan(chars: CameraCharacteristics, params: StreamParams, transform: Transform): CapturePlan {
            val map = chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
                ?: throw IllegalArgumentException("Camera has no stream configurations")
            val maxDuration = params.interval * 100L
            fun fastEnough(format: Int, size: Size) =
                map.getOutputMinFrameDuration(format, size) in 1..maxDuration

            val out = Size(params.width, params.height)
            if (params.kind == FormatKind.MJPEG && transform.isIdentity &&
                map.getOutputSizes(ImageFormat.JPEG)?.contains(out) == true &&
                fastEnough(ImageFormat.JPEG, out)
            ) {
                return CapturePlan(EncodeMode.HW_JPEG, params.width, params.height)
            }

            val mode = if (params.kind == FormatKind.YUYV) EncodeMode.YUYV else EncodeMode.YUV_JPEG
            val yuvSizes = map.getOutputSizes(ImageFormat.YUV_420_888).orEmpty()
                .filter { fastEnough(ImageFormat.YUV_420_888, it) }
                .map { it.width to it.height }
            val (w, h) = chooseCaptureSize(yuvSizes, params.width, params.height, transform)
                ?: throw IllegalArgumentException(
                    "Can't rotate ${transform.rotation.degrees}° at ${params.width}x${params.height} " +
                        "with this camera")
            return CapturePlan(mode, w, h)
        }

        /**
         * The smallest capture size whose transformed image covers the output:
         * the output size itself for 0°/180° when available, and for 90°/270°
         * one whose short side covers the output width (the center band of
         * the rotated image is used, at full resolution).
         */
        fun chooseCaptureSize(
            sizes: List<Pair<Int, Int>>,
            outWidth: Int,
            outHeight: Int,
            transform: Transform,
        ): Pair<Int, Int>? = sizes
            .filter { (w, h) ->
                val (rw, rh) = transform.rotatedSize(w, h)
                rw >= outWidth && rh >= outHeight
            }
            .minByOrNull { (w, h) -> w.toLong() * h }

        /**
         * Map UVC brightness (0..255, 128 = no change) onto the camera's AE
         * compensation range [lower, upper] (in compensation steps), linearly
         * on each side of 128 so the default is exactly 0.
         */
        fun brightnessToCompensation(brightness: Int, lower: Int, upper: Int): Int {
            val b = brightness.coerceIn(0, 255)
            return when {
                b >= 128 -> Math.round((b - 128) / 127.0 * upper).toInt()
                else -> Math.round((128 - b) / 128.0 * lower).toInt()
            }
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

    /** The stream being produced. */
    private class Active(
        val params: StreamParams,
        var transform: Transform,
        val plan: CapturePlan,
        val sink: FrameSink,
        val onError: (String) -> Unit,
    )

    private val thread = HandlerThread("camera").apply { start() }
    private val handler = Handler(thread.looper)
    private val executor = Executor { handler.post(it) }

    private var device: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var reader: ImageReader? = null
    private var encoder: FrameEncoder? = null
    private var active: Active? = null
    private var requestBuilder: CaptureRequest.Builder? = null
    private var aeCompensationRange: Range<Int>? = null

    /** Host brightness, applied as AE compensation; kept across camera restarts. */
    private var brightness = BRIGHTNESS_DEFAULT

    /** Apply the host's brightness control, now and to future streams. */
    fun setBrightness(value: Int) {
        handler.post {
            brightness = value
            val builder = requestBuilder ?: return@post
            val session = session ?: return@post
            applyBrightness(builder)
            session.setSingleRepeatingRequest(builder.build(), executor,
                object : CameraCaptureSession.CaptureCallback() {})
        }
    }

    private fun applyBrightness(builder: CaptureRequest.Builder) {
        val range = aeCompensationRange ?: return
        val compensation = brightnessToCompensation(brightness, range.lower, range.upper)
        builder.set(CaptureRequest.CONTROL_AE_EXPOSURE_COMPENSATION, compensation)
        Log.i(TAG, "Brightness $brightness -> AE compensation $compensation (range $range)")
    }

    /** Start (or restart) producing frames in [params] with [transform]. */
    fun start(params: StreamParams, transform: Transform, sink: FrameSink, onError: (String) -> Unit) {
        handler.post {
            val current = active
            if (current != null && current.params == params && current.transform == transform) {
                return@post
            }
            closeCamera()
            open(params, transform, sink, onError)
        }
    }

    /**
     * Change the transform while streaming. It takes effect at the next frame
     * if the capture setup stays the same; otherwise only the camera restarts
     * (the host's stream continues, briefly repeating the last frame).
     */
    fun setTransform(transform: Transform) {
        handler.post {
            val current = active ?: return@post
            if (current.transform == transform) {
                return@post
            }
            val newPlan = try {
                plan(manager.getCameraCharacteristics(cameraId), current.params, transform)
            } catch (e: IllegalArgumentException) {
                Log.w(TAG, "Can't apply $transform", e)
                current.onError(e.message ?: e.toString())
                return@post
            }

            if (newPlan == current.plan) {
                Log.i(TAG, "Applying $transform live")
                current.transform = transform
                encoder?.setTransform(transform)
            } else {
                Log.i(TAG, "Restarting camera for $transform ($newPlan)")
                closeCamera()
                open(current.params, transform, current.sink, current.onError)
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

    private fun open(params: StreamParams, transform: Transform, sink: FrameSink, onError: (String) -> Unit) {
        try {
            openCamera(params, transform, sink, onError)
        } catch (e: Exception) {
            Log.e(TAG, "Failed to start camera", e)
            closeCamera()
            onError(e.message ?: e.toString())
        }
    }

    @SuppressLint("MissingPermission") // Checked before the service starts.
    private fun openCamera(
        params: StreamParams,
        transform: Transform,
        sink: FrameSink,
        onError: (String) -> Unit,
    ) {
        val chars = manager.getCameraCharacteristics(cameraId)
        val plan = plan(chars, params, transform)
        val fps = Math.round(params.fps).toInt()
        Log.i(TAG, "Starting camera $cameraId: $params with $transform via $plan")

        val format = if (plan.mode == EncodeMode.HW_JPEG) ImageFormat.JPEG else ImageFormat.YUV_420_888
        val reader = ImageReader.newInstance(plan.width, plan.height, format, MAX_IMAGES)
        val realtimeTimestamps = chars.get(CameraCharacteristics.SENSOR_INFO_TIMESTAMP_SOURCE) ==
            CameraCharacteristics.SENSOR_INFO_TIMESTAMP_SOURCE_REALTIME
        val encoder = FrameEncoder(plan, params, transform, realtimeTimestamps, sink)
        reader.setOnImageAvailableListener(encoder, handler)
        this.encoder = encoder
        this.reader = reader
        active = Active(params, transform, plan, sink, onError)

        manager.openCamera(cameraId, executor, object : CameraDevice.StateCallback() {
            override fun onOpened(camera: CameraDevice) {
                if (this@CameraStreamer.reader !== reader) {
                    // Stopped or restarted while opening.
                    camera.close()
                    return
                }
                device = camera
                createSession(camera, reader, chars, plan.mode, fps, onError)
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

                    aeCompensationRange =
                        chars.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_RANGE)
                    // PREVIEW rather than RECORD: it is tuned for latency, and
                    // video stabilization would buffer several frames.
                    val builder = camera.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW).apply {
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
                    }
                    if (brightness != BRIGHTNESS_DEFAULT) {
                        applyBrightness(builder)
                    }
                    requestBuilder = builder
                    val request = builder.build()

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
        requestBuilder = null
        reader?.close()
        reader = null
        encoder?.close()
        encoder = null
        if (active != null) {
            Log.i(TAG, "Camera $cameraId closed")
        }
        active = null
    }
}

/** Plane copies of one camera image, recycled between frames. */
private class PlaneSet {
    var y: Plane? = null
    var u: Plane? = null
    var v: Plane? = null

    fun fill(image: Image) {
        val planes = image.planes
        y = Plane.from(planes[0], y?.data)
        u = Plane.from(planes[1], u?.data)
        v = Plane.from(planes[2], v?.data)
    }
}

/** Per-worker conversion buffers. */
private class WorkerBuffers(width: Int, height: Int, mode: EncodeMode) {
    val nv21 = if (mode == EncodeMode.YUV_JPEG) ByteArray(width * height * 3 / 2) else null
    val jpegOut = ByteArrayOutputStream(width * height / 4)
}

/**
 * Turns each camera image into a frame for the host.
 *
 * Hardware JPEG frames are just copied, on the camera thread. YUV frames are
 * copied out of the image on the camera thread (so the camera can reuse it)
 * and transformed/compressed on [WORKERS] threads: software JPEG at 1080p
 * takes longer than a frame interval on one core. Frames that finish out of
 * order are dropped so the host never goes backwards.
 */
private class FrameEncoder(
    private val plan: CapturePlan,
    params: StreamParams,
    transform: Transform,
    /** Image timestamps are in the elapsedRealtime (boot time) clock. */
    private val realtimeTimestamps: Boolean,
    private val sink: FrameSink,
) : ImageReader.OnImageAvailableListener, Closeable {
    private val mode = plan.mode
    private val width = params.width
    private val height = params.height
    private val rect = Rect(0, 0, width, height)

    /** Swapped by [setTransform]; each frame uses whichever is current. */
    @Volatile
    private var xform = makeTransform(transform)

    private val workers = if (mode == EncodeMode.HW_JPEG) {
        null
    } else {
        Executors.newFixedThreadPool(WORKERS) { r -> Thread(r, "frame-encoder") }
    }
    private val buffers = ThreadLocal.withInitial { WorkerBuffers(width, height, mode) }

    /** Free plane sets; one more than the workers so the camera never waits. */
    private val freeSets = ArrayDeque<PlaneSet>().apply {
        repeat(WORKERS + 1) { add(PlaneSet()) }
    }

    private var nextSeq = 0L
    private val lastDelivered = AtomicLong(-1)

    @Volatile
    private var closed = false

    // Stats, logged every STATS_PERIOD_NS.
    private val statsLock = Any()
    private var statsSince = System.nanoTime()
    private var statsFrames = 0
    private var statsTotalNs = 0L
    private var statsMaxNs = 0L
    private var statsBusy = 0
    private var statsOutOfOrder = 0

    private fun makeTransform(transform: Transform) =
        if (mode == EncodeMode.HW_JPEG) null
        else YuvTransform(plan.width, plan.height, width, height, transform)

    /** Use [transform] from the next frame on. The capture plan must not change. */
    fun setTransform(transform: Transform) {
        xform = makeTransform(transform)
    }

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
        val seq = nextSeq++

        if (workers == null) {
            image.use {
                val start = System.nanoTime()
                val buffer = image.planes[0].buffer
                val length = buffer.remaining()
                // A fresh array per frame: the sender may still hold the
                // previous one.
                val data = ByteArray(length)
                buffer.get(data)
                deliver(seq, data, length, ptsUs, System.nanoTime() - start)
            }
            return
        }

        val set = synchronized(freeSets) { freeSets.pollFirst() }
        if (set == null) {
            // All workers busy: drop this camera frame rather than queue it.
            image.close()
            synchronized(statsLock) { statsBusy++ }
            return
        }
        image.use { set.fill(it) }

        val xf = xform!!
        try {
            workers.execute { encode(set, xf, seq, ptsUs) }
        } catch (e: RejectedExecutionException) {
            // Closed meanwhile.
            synchronized(freeSets) { freeSets.addLast(set) }
        }
    }

    private fun encode(set: PlaneSet, xf: YuvTransform, seq: Long, ptsUs: Long) {
        val start = System.nanoTime()
        try {
            if (mode == EncodeMode.YUYV) {
                val out = ByteArray(width * height * 2)
                xf.toYuyv(set.y!!, set.u!!, set.v!!, out)
                deliver(seq, out, out.size, ptsUs, System.nanoTime() - start)
            } else {
                val b = buffers.get()!!
                xf.toNv21(set.y!!, set.u!!, set.v!!, b.nv21!!)
                b.jpegOut.reset()
                YuvImage(b.nv21, ImageFormat.NV21, width, height, null)
                    .compressToJpeg(rect, JPEG_QUALITY, b.jpegOut)
                deliver(seq, b.jpegOut.toByteArray(), b.jpegOut.size(), ptsUs,
                    System.nanoTime() - start)
            }
        } finally {
            synchronized(freeSets) { freeSets.addLast(set) }
        }
    }

    private fun deliver(seq: Long, data: ByteArray, length: Int, ptsUs: Long, elapsedNs: Long) {
        if (closed) {
            return
        }
        while (true) {
            val last = lastDelivered.get()
            if (seq <= last) {
                synchronized(statsLock) { statsOutOfOrder++ }
                return
            }
            if (lastDelivered.compareAndSet(last, seq)) {
                break
            }
        }
        sink.onFrame(data, length, ptsUs)
        recordStats(elapsedNs)
    }

    override fun close() {
        closed = true
        workers?.shutdown()
    }

    private fun recordStats(elapsedNs: Long) = synchronized(statsLock) {
        statsFrames++
        statsTotalNs += elapsedNs
        statsMaxNs = maxOf(statsMaxNs, elapsedNs)

        val now = System.nanoTime()
        if (now - statsSince >= STATS_PERIOD_NS) {
            val seconds = (now - statsSince) / 1e9
            Log.i(TAG, "Encode $mode ${plan.width}x${plan.height} -> ${width}x$height: " +
                "%.1f fps, avg %.1f ms, max %.1f ms, busy drops %d, out of order %d".format(
                    statsFrames / seconds,
                    statsTotalNs / 1e6 / statsFrames,
                    statsMaxNs / 1e6,
                    statsBusy,
                    statsOutOfOrder,
                ))
            statsSince = now
            statsFrames = 0
            statsTotalNs = 0
            statsMaxNs = 0
            statsBusy = 0
            statsOutOfOrder = 0
        }
    }

    companion object {
        private const val TAG = "FrameEncoder"
        private const val JPEG_QUALITY = 85
        private const val STATS_PERIOD_NS = 5_000_000_000L

        /** Software JPEG at 1080p takes ~38 ms on the OnePlus 15; two fit 30 fps. */
        private const val WORKERS = 2
    }
}

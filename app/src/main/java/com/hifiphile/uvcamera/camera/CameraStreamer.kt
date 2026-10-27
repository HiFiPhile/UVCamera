// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import android.annotation.SuppressLint
import android.graphics.ImageFormat
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CameraMetadata
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.CaptureResult
import android.hardware.camera2.TotalCaptureResult
import android.hardware.camera2.params.ColorSpaceTransform
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.RggbChannelVector
import android.hardware.camera2.params.SessionConfiguration
import android.media.Image
import android.media.ImageReader
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import android.util.Log
import android.util.Range
import android.util.Size
import com.hifiphile.uvcamera.daemon.ControlIds
import com.hifiphile.uvcamera.daemon.FormatKind
import com.hifiphile.uvcamera.daemon.StreamParams
import com.hifiphile.uvcamera.webcam.AWB_PRESET_KELVIN
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

    /** YUV from the camera, transformed and compressed by [JpegEncoder]. */
    YUV_JPEG,
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
class CameraStreamer(private val manager: CameraManager, val cameraId: String) {
    companion object {
        private const val TAG = "CameraStreamer"
        private const val JPEG_QUALITY = 85
        private const val MAX_IMAGES = 3
        private const val RELEASE_TIMEOUT_MS = 2000L

        /**
         * How to produce [params] with [transform]: software JPEG from YUV,
         * captured large enough that the transformed image covers the
         * output. The camera's JPEG encoder is only a fallback for cameras
         * without a usable YUV size: its output is noisier (the YUV stream
         * gets more noise filtering) and it can't rotate or mirror.
         *
         * @throws IllegalArgumentException if no capture size works
         */
        fun plan(chars: CameraCharacteristics, params: StreamParams, transform: Transform): CapturePlan {
            require(params.kind == FormatKind.MJPEG) { "Only MJPEG is supported" }
            val map = chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
                ?: throw IllegalArgumentException("Camera has no stream configurations")
            val maxDuration = maxFrameDurationNs(params.interval)
            fun fastEnough(format: Int, size: Size) =
                map.getOutputMinFrameDuration(format, size) in 1..maxDuration

            val yuvSizes = map.getOutputSizes(ImageFormat.YUV_420_888).orEmpty()
                .filter { fastEnough(ImageFormat.YUV_420_888, it) }
                .map { it.width to it.height }
            chooseCaptureSize(yuvSizes, params.width, params.height, transform)?.let { (w, h) ->
                return CapturePlan(EncodeMode.YUV_JPEG, w, h)
            }

            val out = Size(params.width, params.height)
            if (transform.isIdentity &&
                map.getOutputSizes(ImageFormat.JPEG)?.contains(out) == true &&
                fastEnough(ImageFormat.JPEG, out)
            ) {
                return CapturePlan(EncodeMode.HW_JPEG, params.width, params.height)
            }
            throw IllegalArgumentException(
                "Can't rotate ${transform.rotation.degrees}° at ${params.width}x${params.height} " +
                    "with this camera")
        }

        /**
         * Longest camera frame duration that keeps up with a UVC frame
         * [interval] (100 ns units). Intervals are truncated, 333333 for
         * 30 fps, while cameras report 33333333 ns: allow the lost fraction.
         */
        fun maxFrameDurationNs(interval: Int): Long = interval * 100L + 99

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

        /** Zoom ratio for a UVC zoom value in hundredths, within the camera's range. */
        fun zoomRatio(value: Int, lower: Float, upper: Float): Float =
            (value / 100f).coerceIn(lower, upper)

        /**
         * Focus distance in diopters for a UVC focus value: 0 is the nearest
         * the lens can focus ([minFocusDiopters]), [ControlIds.FOCUS_MAX] is
         * infinity (0 diopters).
         */
        fun focusDiopters(value: Int, minFocusDiopters: Float): Float =
            minFocusDiopters * (1f - value.coerceIn(0, ControlIds.FOCUS_MAX).toFloat() / ControlIds.FOCUS_MAX)

        /** The available AWB preset whose nominal temperature is closest to [kelvin]. */
        fun awbPreset(kelvin: Int, available: List<Int>, presets: Map<Int, Int>): Int =
            presets.filterKeys { it in available }
                .minByOrNull { (_, k) -> Math.abs(k - kelvin) }
                ?.key ?: CameraMetadata.CONTROL_AWB_MODE_AUTO

        /** AE antibanding mode for a UVC power line frequency value. */
        fun antibanding(value: Int, available: List<Int>): Int {
            val mode = when (value) {
                0 -> CameraMetadata.CONTROL_AE_ANTIBANDING_MODE_OFF
                1 -> CameraMetadata.CONTROL_AE_ANTIBANDING_MODE_50HZ
                2 -> CameraMetadata.CONTROL_AE_ANTIBANDING_MODE_60HZ
                else -> CameraMetadata.CONTROL_AE_ANTIBANDING_MODE_AUTO
            }
            return if (mode in available) mode else CameraMetadata.CONTROL_AE_ANTIBANDING_MODE_AUTO
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

    /** A stream the camera couldn't start; retried when the transform changes. */
    private class Failed(val params: StreamParams, val sink: FrameSink, val onError: (String) -> Unit)

    private var failed: Failed? = null
    private var requestBuilder: CaptureRequest.Builder? = null
    private val chars by lazy { manager.getCameraCharacteristics(cameraId) }
    private val colorTemperatureRange by lazy { colorTemperatureRange(chars) }
    private val calibration by lazy { colorCalibration(chars) }

    /**
     * The colour correction matrix the camera last chose under AWB, used with
     * manual white balance gains so colours keep the camera's look.
     */
    private var autoColorTransform: ColorSpaceTransform? = null

    /**
     * Control values the host set ([ControlIds] to value); kept across camera
     * restarts. Unset controls keep the camera's defaults.
     */
    private val controls = mutableMapOf<Int, Int>()

    /** Apply a host control, now and to future streams. */
    fun setControl(id: Int, value: Int) {
        handler.post {
            controls[id] = value
            val builder = requestBuilder ?: return@post
            val session = session ?: return@post
            applyControls(builder)
            session.setSingleRepeatingRequest(builder.build(), executor, resultLogger)
        }
    }

    /**
     * Logs what the camera actually applied for the host's controls,
     * whenever it changes, to tell a HAL that ignores a setting from one we
     * didn't send.
     */
    private val resultLogger = object : CameraCaptureSession.CaptureCallback() {
        private var last = ""

        override fun onCaptureCompleted(
            s: CameraCaptureSession,
            request: CaptureRequest,
            result: TotalCaptureResult,
        ) {
            val awb = result.get(CaptureResult.CONTROL_AWB_MODE)
            if (awb == CaptureResult.CONTROL_AWB_MODE_AUTO) {
                result.get(CaptureResult.COLOR_CORRECTION_TRANSFORM)?.let { autoColorTransform = it }
            }

            // Gains under AWB and the focus distance under AF change every
            // frame; only log them when they are manual.
            val af = result.get(CaptureResult.CONTROL_AF_MODE)
            // Under CCT some HALs report a vendor AWB mode rather than OFF.
            val manualWb = awb != CaptureResult.CONTROL_AWB_MODE_AUTO
            val gains = result.get(CaptureResult.COLOR_CORRECTION_GAINS)?.takeIf { manualWb }
            val focus = result.get(CaptureResult.LENS_FOCUS_DISTANCE)
                ?.takeIf { af == CaptureResult.CONTROL_AF_MODE_OFF }
            val cct = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.BAKLAVA && manualWb) {
                result.get(CaptureResult.COLOR_CORRECTION_COLOR_TEMPERATURE)
            } else {
                null
            }
            val summary = "awb=$awb" + (cct?.let { " cct=${it}K" } ?: "") +
                (gains?.let { " gains=%.2f/%.2f/%.2f".format(it.red, it.greenEven, it.blue) } ?: "") +
                " af=$af" + (focus?.let { " focus=%.2f".format(it) } ?: "") +
                " zoom=${result.get(CaptureResult.CONTROL_ZOOM_RATIO)}" +
                " antibanding=${result.get(CaptureResult.CONTROL_AE_ANTIBANDING_MODE)}" +
                " nr=${result.get(CaptureResult.NOISE_REDUCTION_MODE)}" +
                " edge=${result.get(CaptureResult.EDGE_MODE)}"
            if (summary != last) {
                last = summary
                Log.i(TAG, "Applied: $summary")
            }
        }
    }

    /** Turn the host's control values into capture request settings. */
    private fun applyControls(builder: CaptureRequest.Builder) {
        controls[ControlIds.BRIGHTNESS]?.let { b ->
            chars.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_RANGE)?.let { range ->
                builder.set(CaptureRequest.CONTROL_AE_EXPOSURE_COMPENSATION,
                    brightnessToCompensation(b, range.lower, range.upper))
            }
        }
        controls[ControlIds.ZOOM]?.let { z ->
            chars.get(CameraCharacteristics.CONTROL_ZOOM_RATIO_RANGE)?.let { range ->
                builder.set(CaptureRequest.CONTROL_ZOOM_RATIO, zoomRatio(z, range.lower, range.upper))
            }
        }
        // The host sees the controls of every camera; skip what this one lacks.
        val afModes = chars.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES)?.toList().orEmpty()
        val minFocus = chars.get(CameraCharacteristics.LENS_INFO_MINIMUM_FOCUS_DISTANCE) ?: 0f
        if (controls[ControlIds.FOCUS_AUTO] == 0) {
            if (minFocus > 0f && CaptureRequest.CONTROL_AF_MODE_OFF in afModes) {
                builder.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_OFF)
                builder.set(CaptureRequest.LENS_FOCUS_DISTANCE,
                    focusDiopters(controls[ControlIds.FOCUS] ?: ControlIds.FOCUS_MAX, minFocus))
            }
        } else if (controls.containsKey(ControlIds.FOCUS_AUTO)) {
            if (CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO in afModes) {
                builder.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO)
            }
        }
        if (controls[ControlIds.WHITE_BALANCE_AUTO] == 0) {
            val kelvin = controls[ControlIds.WHITE_BALANCE_TEMPERATURE] ?: 5000
            val calibration = calibration
            val cctRange = colorTemperatureRange
            if (cctRange != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.BAKLAVA) {
                // The camera converts the temperature with its own tuning.
                builder.set(CaptureRequest.CONTROL_AWB_MODE, CaptureRequest.CONTROL_AWB_MODE_OFF)
                builder.set(CaptureRequest.COLOR_CORRECTION_MODE, CaptureRequest.COLOR_CORRECTION_MODE_CCT)
                builder.set(CaptureRequest.COLOR_CORRECTION_COLOR_TEMPERATURE, cctRange.clamp(kelvin))
                builder.set(CaptureRequest.COLOR_CORRECTION_COLOR_TINT, 0)
            } else if (calibration != null) {
                // Many HALs accept the AWB presets but keep auto gains, so
                // set the gains for the temperature ourselves.
                val (r, g, b) = calibration.gains(kelvin.toDouble()).map { it.toFloat() }
                builder.set(CaptureRequest.CONTROL_AWB_MODE, CaptureRequest.CONTROL_AWB_MODE_OFF)
                builder.set(CaptureRequest.COLOR_CORRECTION_MODE,
                    CaptureRequest.COLOR_CORRECTION_MODE_TRANSFORM_MATRIX)
                builder.set(CaptureRequest.COLOR_CORRECTION_GAINS, RggbChannelVector(r, g, g, b))
                builder.set(CaptureRequest.COLOR_CORRECTION_TRANSFORM, autoColorTransform
                    ?: calibration.colorTransform(kelvin.toDouble()).toColorSpaceTransform())
                Log.i(TAG, "White balance ${kelvin}K: gains %.2f/%.2f/%.2f, %s matrix".format(
                    r, g, b, if (autoColorTransform != null) "AWB" else "calibrated"))
            } else {
                val available = chars.get(CameraCharacteristics.CONTROL_AWB_AVAILABLE_MODES)?.toList().orEmpty()
                builder.set(CaptureRequest.CONTROL_AWB_MODE, awbPreset(kelvin, available, AWB_PRESET_KELVIN))
            }
        } else if (controls.containsKey(ControlIds.WHITE_BALANCE_AUTO)) {
            builder.set(CaptureRequest.CONTROL_AWB_MODE, CaptureRequest.CONTROL_AWB_MODE_AUTO)
            if (colorTemperatureRange != null || calibration != null) {
                builder.set(CaptureRequest.COLOR_CORRECTION_MODE, CaptureRequest.COLOR_CORRECTION_MODE_FAST)
            }
        }
        controls[ControlIds.POWER_LINE_FREQUENCY]?.let { p ->
            val available = chars.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_ANTIBANDING_MODES)
                ?.toList().orEmpty()
            builder.set(CaptureRequest.CONTROL_AE_ANTIBANDING_MODE, antibanding(p, available))
        }
        Log.i(TAG, "Controls: $controls")
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
            val current = active
            if (current == null) {
                failed?.let {
                    Log.i(TAG, "Retrying the stream with $transform")
                    open(it.params, transform, it.sink, it.onError)
                }
                return@post
            }
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
        handler.post {
            closeCamera()
            failed = null
        }
    }

    /** Stop and release the handler thread. */
    fun release() {
        handler.post {
            closeCamera()
            thread.quitSafely()
        }
    }

    /**
     * [release], and wait for the camera to be closed, so another camera
     * can be opened: phones may not run two at once.
     */
    fun releaseAndWait() {
        release()
        thread.join(RELEASE_TIMEOUT_MS)
    }

    private fun open(params: StreamParams, transform: Transform, sink: FrameSink, onError: (String) -> Unit) {
        failed = null
        try {
            openCamera(params, transform, sink, onError)
        } catch (e: Exception) {
            Log.e(TAG, "Failed to start camera", e)
            closeCamera()
            failed = Failed(params, sink, onError)
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
                    if (controls.isNotEmpty()) {
                        applyControls(builder)
                    }
                    requestBuilder = builder
                    val request = builder.build()

                    s.setSingleRepeatingRequest(request, executor, resultLogger)
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

/**
 * Turns each camera image into a frame for the host.
 *
 * Hardware JPEG frames are just copied, on the camera thread. YUV frames are
 * copied out of the image on the camera thread (so the camera can reuse it)
 * and transformed/compressed on [WORKERS] threads (twice that above 30 fps):
 * software JPEG at 1080p
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

    /** Swapped by [setTransform]; each frame uses whichever is current. */
    @Volatile
    private var xform = makeTransform(transform)

    /** Enough workers for the frame rate: software JPEG at 1080p takes ~38 ms. */
    private val workerCount = if (params.fps > 31) WORKERS * 2 else WORKERS
    private val workers = if (mode == EncodeMode.HW_JPEG) {
        null
    } else {
        Executors.newFixedThreadPool(workerCount) { r -> Thread(r, "frame-encoder") }
    }

    /** Free plane sets; one more than the workers so the camera never waits. */
    private val freeSets = ArrayDeque<PlaneSet>().apply {
        repeat(workerCount + 1) { add(PlaneSet()) }
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
            val jpeg = JpegEncoder.encode(xf, set.y!!, set.u!!, set.v!!, JPEG_QUALITY)
            deliver(seq, jpeg, jpeg.size, ptsUs, System.nanoTime() - start)
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

/** A row-major 3x3 matrix as camera metadata, in 1/10000ths. */
private fun DoubleArray.toColorSpaceTransform() =
    ColorSpaceTransform(IntArray(18) { if (it % 2 == 0) Math.round(this[it / 2] * 10000).toInt() else 10000 })

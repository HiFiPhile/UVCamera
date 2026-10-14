// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.webcam

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.hardware.camera2.CameraManager
import android.os.IBinder
import android.util.Log
import com.hifiphile.uvcamera.MainActivity
import com.hifiphile.uvcamera.R
import com.hifiphile.uvcamera.camera.CameraStreamer
import com.hifiphile.uvcamera.camera.Transform
import com.hifiphile.uvcamera.camera.queryCameras
import com.hifiphile.uvcamera.daemon.Client
import com.hifiphile.uvcamera.daemon.SourceChannel
import com.hifiphile.uvcamera.daemon.SourceEvent
import com.hifiphile.uvcamera.daemon.StreamParams
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/** What the webcam service is doing, for the UI. */
data class WebcamState(
    val running: Boolean = false,
    /** Camera feeding the host, e.g. "0 (back)". */
    val camera: String? = null,
    /** The stream the camera is currently producing. */
    val streaming: StreamParams? = null,
    val error: String? = null,
)

/**
 * Foreground service (type camera) that feeds the host from the camera.
 *
 * It must be started while the app is visible: Android only lets a
 * foreground service use the camera if it was started from the foreground.
 * It then keeps that access while running, enables the daemon, attaches as
 * the frame source, and opens the camera only while the host is streaming.
 */
class WebcamService : Service() {
    companion object {
        private const val TAG = "WebcamService"
        private const val ACTION_START = "com.hifiphile.uvcamera.action.START"
        private const val ACTION_STOP = "com.hifiphile.uvcamera.action.STOP"
        private const val CHANNEL_ID = "webcam"
        private const val NOTIFICATION_ID = 1
        private const val RETRY_DELAY_MS = 1000L

        private val _state = MutableStateFlow(WebcamState())
        val state: StateFlow<WebcamState> = _state.asStateFlow()

        fun start(context: Context) {
            context.startForegroundService(
                Intent(context, WebcamService::class.java).setAction(ACTION_START))
        }

        /** Stop the camera and disable the webcam. Works even if not running. */
        fun stop(context: Context) {
            context.startService(Intent(context, WebcamService::class.java).setAction(ACTION_STOP))
        }
    }

    private var worker: Thread? = null

    @Volatile
    private var stopping = false

    @Volatile
    private var channel: SourceChannel? = null

    /** Id of the host stream the camera is producing for. */
    @Volatile
    private var streamId = 0

    /** Current rotation/mirroring, kept up to date from the settings. */
    @Volatile
    private var transform = Transform()

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_START -> startWebcam()
            ACTION_STOP -> stopWebcam()
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        stopping = true
        channel?.close()
        _state.value = WebcamState()
        super.onDestroy()
    }

    private fun startWebcam() {
        startForeground(
            NOTIFICATION_ID,
            buildNotification(),
            ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA,
        )
        if (worker != null) {
            return
        }
        stopping = false
        _state.value = WebcamState(running = true)
        worker = Thread(::run, "webcam").apply { start() }
    }

    private fun stopWebcam() {
        stopping = true
        channel?.close()
        // Network/IPC off the main thread.
        Thread {
            try {
                Client().use { it.disable() }
            } catch (e: Exception) {
                Log.w(TAG, "Failed to disable webcam", e)
            }
        }.start()
        _state.value = WebcamState()
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun run() {
        val manager = getSystemService(CameraManager::class.java)
        val settings = WebcamSettings(this)
        val camera = try {
            WebcamSettings.pickCamera(queryCameras(manager), settings.cameraId)
                ?: throw IllegalStateException("No cameras found")
        } catch (e: Exception) {
            Log.e(TAG, "No usable camera", e)
            _state.update { it.copy(error = e.toString()) }
            return
        }
        val formats = webcamFormats(camera, settings.maxResolution.height)
        val controls = webcamControls(camera)
        fun currentTransform() = WebcamSettings.transformFor(
            camera, settings.rotation, settings.mirrorH, settings.mirrorV)
        transform = currentTransform()
        val streamer = CameraStreamer(manager, camera.id)
        _state.update { it.copy(camera = "${camera.id} (${camera.facing.name.lowercase()})") }
        Log.i(TAG, "Using camera ${camera.id} (sensor ${camera.sensorOrientation}°, " +
            "$transform), advertising $formats and $controls")

        // Rotation and mirroring apply live; the camera and formats don't.
        val listener = settings.onTransformChanged {
            transform = currentTransform()
            Log.i(TAG, "Transform changed: $transform")
            _state.update { it.copy(error = null) }
            streamer.setTransform(transform)
        }

        while (!stopping) {
            try {
                Client().use { client ->
                    // Re-enabling would re-enumerate the device on the host.
                    if (!client.getStatus().enabled) {
                        client.enable(formats, controls)
                    }
                }
                val client = Client()
                val channel = try {
                    client.attachSource()
                } catch (e: Exception) {
                    client.close()
                    throw e
                }
                serve(channel, streamer)
            } catch (e: Exception) {
                if (stopping) {
                    break
                }
                Log.w(TAG, "Daemon connection failed; retrying", e)
                _state.update { it.copy(streaming = null, error = e.message ?: e.toString()) }
                Thread.sleep(RETRY_DELAY_MS)
            }
        }

        settings.removeListener(listener)
        streamer.release()
    }

    /** Follow the host's streams until the channel closes. */
    private fun serve(channel: SourceChannel, streamer: CameraStreamer) {
        this.channel = channel
        if (stopping) {
            channel.close()
            return
        }
        val sender = FrameSender(channel) { channel.close() }
        _state.update { it.copy(error = null) }

        try {
            while (true) {
                when (val event = channel.readEvent()) {
                    is SourceEvent.StreamStart -> {
                        Log.i(TAG, "Host started stream ${event.streamId}: ${event.params}")
                        streamId = event.streamId
                        _state.update { it.copy(streaming = event.params, error = null) }
                        streamer.start(
                            event.params,
                            transform,
                            { data, length, ptsUs -> sender.offer(streamId, data, length, ptsUs) },
                            { error -> _state.update { it.copy(error = error) } },
                        )
                    }
                    is SourceEvent.Control -> {
                        Log.i(TAG, "Host set control ${event.control} to ${event.value}")
                        streamer.setControl(event.control, event.value)
                    }
                    is SourceEvent.StreamStop -> {
                        Log.i(TAG, "Host stopped stream ${event.streamId}")
                        streamer.stop()
                        _state.update { it.copy(streaming = null) }
                    }
                }
            }
        } finally {
            streamer.stop()
            sender.close()
            channel.close()
            _state.update { it.copy(streaming = null) }
        }
    }

    private fun buildNotification(): Notification {
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(NotificationChannel(
            CHANNEL_ID,
            getString(R.string.notification_channel_webcam),
            NotificationManager.IMPORTANCE_LOW,
        ))

        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val stop = PendingIntent.getService(
            this, 0, Intent(this, WebcamService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE)

        return Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(getString(R.string.notification_title))
            .setContentText(getString(R.string.notification_text))
            .setContentIntent(open)
            .setOngoing(true)
            .addAction(Notification.Action.Builder(
                null, getString(R.string.notification_stop), stop).build())
            .build()
    }
}

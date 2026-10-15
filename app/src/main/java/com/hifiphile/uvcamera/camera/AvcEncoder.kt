// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.view.Surface
import java.io.Closeable

/** Receives encoded H.264 access units (Annex B), in decoding order. */
fun interface AvcSink {
    fun onAccessUnit(data: ByteArray, ptsUs: Long, keyFrame: Boolean)
}

/**
 * H.264 encoding of one stream with the platform's (hardware) encoder:
 * frames drawn into [inputSurface] in, Annex B access units out, with the
 * surface frames' presentation times. Tuned for a live webcam: no B-frames
 * (frames go out in capture order), low latency, an IDR frame every second
 * and on request, each preceded by its SPS and PPS so a host can start
 * decoding at any IDR.
 */
class AvcEncoder(
    width: Int,
    height: Int,
    fps: Int,
    private val sink: AvcSink,
) : Closeable {
    companion object {
        private const val TAG = "AvcEncoder"
        private const val MIME = MediaFormat.MIMETYPE_VIDEO_AVC

        /** Bits per pixel per frame: about 12 Mb/s at 1080p30. */
        private const val BITS_PER_PIXEL = 0.2

        fun bitrate(width: Int, height: Int, fps: Int): Int =
            (width.toLong() * height * fps * BITS_PER_PIXEL).toInt()

        /** NAL unit types in an Annex B buffer. */
        fun nalTypes(data: ByteArray, length: Int = data.size): List<Int> {
            val types = mutableListOf<Int>()
            var zeros = 0
            for (i in 0 until length) {
                val b = data[i].toInt() and 0xff
                if (b == 1 && zeros >= 2 && i + 1 < length) {
                    types += data[i + 1].toInt() and 0x1f
                }
                zeros = if (b == 0) zeros + 1 else 0
            }
            return types
        }

        private const val NAL_SPS = 7
    }

    private val thread = HandlerThread("avc-encoder").apply { start() }
    private val codec = MediaCodec.createEncoderByType(MIME)

    /** Where frames to encode are drawn. */
    val inputSurface: Surface

    /** SPS and PPS, for encoders that don't repeat them before IDR frames. */
    @Volatile
    private var codecConfig: ByteArray? = null

    @Volatile
    private var closed = false

    init {
        val format = MediaFormat.createVideoFormat(MIME, width, height).apply {
            setInteger(MediaFormat.KEY_COLOR_FORMAT,
                MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
            setInteger(MediaFormat.KEY_BIT_RATE, bitrate(width, height, fps))
            setInteger(MediaFormat.KEY_FRAME_RATE, fps)
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1)
            setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0)
            setInteger(MediaFormat.KEY_PREPEND_HEADER_TO_SYNC_FRAMES, 1)
            setInteger(MediaFormat.KEY_LATENCY, 1)
            // Realtime.
            setInteger(MediaFormat.KEY_PRIORITY, 0)
            val caps = codec.codecInfo.getCapabilitiesForType(MIME)
            if (caps.encoderCapabilities?.isBitrateModeSupported(
                    MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_VBR) == true
            ) {
                setInteger(MediaFormat.KEY_BITRATE_MODE,
                    MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_VBR)
            }
            if (caps.profileLevels.any { it.profile == MediaCodecInfo.CodecProfileLevel.AVCProfileHigh }) {
                setInteger(MediaFormat.KEY_PROFILE, MediaCodecInfo.CodecProfileLevel.AVCProfileHigh)
            }
        }
        codec.setCallback(Callback(), Handler(thread.looper))
        codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        inputSurface = codec.createInputSurface()
        codec.start()
        Log.i(TAG, "Started ${codec.name}: $format")
    }

    private inner class Callback : MediaCodec.Callback() {
        override fun onInputBufferAvailable(codec: MediaCodec, index: Int) {
            // Input comes through the surface.
        }

        override fun onOutputBufferAvailable(codec: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
            if (closed) {
                return
            }
            val buffer = codec.getOutputBuffer(index)
            if (buffer == null || info.size == 0) {
                codec.releaseOutputBuffer(index, false)
                return
            }
            buffer.position(info.offset)
            buffer.limit(info.offset + info.size)
            val data = ByteArray(info.size)
            buffer.get(data)
            codec.releaseOutputBuffer(index, false)

            if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) {
                codecConfig = data
                Log.i(TAG, "Codec config: " + data.joinToString("") { "%02x".format(it) })
                return
            }
            val keyFrame = info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0
            val config = codecConfig
            val unit = if (keyFrame && config != null && NAL_SPS !in nalTypes(data)) {
                config + data
            } else {
                data
            }
            sink.onAccessUnit(unit, info.presentationTimeUs, keyFrame)
        }

        override fun onError(codec: MediaCodec, e: MediaCodec.CodecException) {
            Log.e(TAG, "Encoder error", e)
        }

        override fun onOutputFormatChanged(codec: MediaCodec, format: MediaFormat) {
            Log.i(TAG, "Output format: $format")
        }
    }

    /** Make the next frame an IDR frame. */
    fun requestKeyFrame() {
        if (closed) {
            return
        }
        codec.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
    }

    override fun close() {
        closed = true
        runCatching { codec.stop() }
        codec.release()
        inputSurface.release()
        thread.quitSafely()
    }
}

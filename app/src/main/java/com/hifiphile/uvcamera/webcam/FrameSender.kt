// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.webcam

import android.util.Log
import com.hifiphile.uvcamera.daemon.SourceChannel
import java.io.IOException

/**
 * Sends frames to the daemon from its own thread, keeping at most one
 * pending frame: if the socket is slower than the camera, stale frames are
 * replaced rather than queued, so latency stays at one frame.
 */
class FrameSender(private val channel: SourceChannel, private val onFailure: (IOException) -> Unit) {
    private class Pending(val streamId: Int, val data: ByteArray, val length: Int, val ptsUs: Long)

    private val lock = Object()
    private var pending: Pending? = null
    private var closed = false

    private val thread = Thread(::run, "frame-sender").apply { start() }

    fun offer(streamId: Int, data: ByteArray, length: Int, ptsUs: Long) {
        synchronized(lock) {
            if (closed) {
                return
            }
            pending = Pending(streamId, data, length, ptsUs)
            lock.notifyAll()
        }
    }

    fun close() {
        synchronized(lock) {
            closed = true
            pending = null
            lock.notifyAll()
        }
        thread.join(1000)
    }

    private fun run() {
        while (true) {
            val frame = synchronized(lock) {
                while (pending == null && !closed) {
                    lock.wait()
                }
                if (closed) {
                    return
                }
                pending.also { pending = null }!!
            }

            try {
                channel.sendFrame(frame.streamId, frame.ptsUs, frame.data, frame.length)
            } catch (e: IOException) {
                Log.w(TAG, "Failed to send frame", e)
                synchronized(lock) { closed = true }
                onFailure(e)
                return
            }
        }
    }

    companion object {
        private const val TAG = "FrameSender"
    }
}

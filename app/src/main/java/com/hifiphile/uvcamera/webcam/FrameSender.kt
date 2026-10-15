// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.webcam

import android.util.Log
import com.hifiphile.uvcamera.daemon.SourceChannel
import java.io.IOException
import java.util.ArrayDeque

/**
 * Sends frames to the daemon from its own thread. Frames that stand alone
 * replace each other: if the socket is slower than the camera, stale frames
 * are dropped rather than queued, so latency stays at one frame. Frames that
 * depend on the previous ones (H.264) queue in order; if the queue backs up,
 * it is dropped up to the next key frame, which [onBacklog] should ask for.
 */
class FrameSender(
    private val channel: SourceChannel,
    private val onBacklog: () -> Unit,
    private val onFailure: (IOException) -> Unit,
) {
    private class Pending(val streamId: Int, val data: ByteArray, val length: Int, val ptsUs: Long)

    private val lock = Object()
    private val queue = ArrayDeque<Pending>()
    private var waitingForKeyFrame = false
    private var closed = false

    private val thread = Thread(::run, "frame-sender").apply { start() }

    /**
     * Queue a frame. [ordered] frames depend on the ones before; [keyFrame]
     * says whether this one starts over.
     */
    fun offer(streamId: Int, data: ByteArray, length: Int, ptsUs: Long, ordered: Boolean, keyFrame: Boolean) {
        var backlog = false
        synchronized(lock) {
            if (closed) {
                return
            }
            if (!ordered) {
                queue.clear()
            } else {
                if (waitingForKeyFrame && !keyFrame) {
                    return
                }
                waitingForKeyFrame = false
                if (queue.size >= MAX_QUEUED && !keyFrame) {
                    Log.w(TAG, "Dropping ${queue.size} queued frames until the next key frame")
                    queue.clear()
                    waitingForKeyFrame = true
                    backlog = true
                }
            }
            if (!backlog) {
                queue.addLast(Pending(streamId, data, length, ptsUs))
                lock.notifyAll()
            }
        }
        if (backlog) {
            onBacklog()
        }
    }

    fun close() {
        synchronized(lock) {
            closed = true
            queue.clear()
            lock.notifyAll()
        }
        thread.join(1000)
    }

    private fun run() {
        while (true) {
            val frame = synchronized(lock) {
                while (queue.isEmpty() && !closed) {
                    lock.wait()
                }
                if (closed) {
                    return
                }
                queue.removeFirst()
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

        /** About a quarter second at 30 fps. */
        private const val MAX_QUEUED = 8
    }
}

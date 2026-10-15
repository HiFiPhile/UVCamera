// SPDX-FileCopyrightText: 2024 Andrew Gunnerson
// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only
// Based on MSD's daemon client.

package com.hifiphile.uvcamera.daemon

import android.net.LocalSocket
import android.net.LocalSocketAddress
import java.io.BufferedOutputStream
import java.io.Closeable
import java.io.IOException

class ClientException(message: String) : Exception(message)

/** Blocking connection to uvcd. Not thread safe. */
class Client : Closeable {
    private val socket = LocalSocket()
    private val reader: Reader
    private val writer: Writer

    init {
        try {
            socket.connect(LocalSocketAddress(SOCKET_NAME, LocalSocketAddress.Namespace.ABSTRACT))
            reader = Reader(socket.inputStream)
            writer = Writer(BufferedOutputStream(socket.outputStream))

            writer.u8(PROTOCOL_VERSION)
            writer.flush()
            // The daemon closes the connection instead of answering if this
            // app isn't authorized or SELinux isn't set up.
            if (reader.u8() != 1) {
                throw IOException("Daemon rejected protocol version $PROTOCOL_VERSION")
            }
        } catch (e: IOException) {
            socket.close()
            throw IOException("Failed to connect to uvcd", e)
        }
    }

    override fun close() = socket.close()

    private fun request(request: Request): Response {
        request.write(writer)
        writer.flush()
        return Response.read(reader)
    }

    private fun expectOk(response: Response) {
        when (response) {
            Response.Ok -> {}
            is Response.Error -> throw ClientException(response.message)
            else -> throw IOException("Unexpected response: $response")
        }
    }

    fun getStatus(): Status = when (val r = request(Request.GetStatus)) {
        is Response.StatusResponse -> r.status
        is Response.Error -> throw ClientException(r.message)
        else -> throw IOException("Unexpected response: $r")
    }

    fun enable(formats: List<FormatSpec>, controls: ControlCaps = ControlCaps(), maxburst: Int = 0) =
        expectOk(request(Request.Enable(maxburst, formats, controls)))

    fun disable() = expectOk(request(Request.Disable))

    /**
     * Turn this connection into the frame source channel. The client must not
     * be used afterwards; close the returned channel instead.
     */
    fun attachSource(): SourceChannel {
        expectOk(request(Request.AttachSource))
        return SourceChannel(socket, reader, writer)
    }
}

/**
 * The frame source side of a connection: blocking [readEvent] on one thread,
 * [sendFrame] from another. Closing it unblocks a pending [readEvent].
 */
class SourceChannel internal constructor(
    private val socket: LocalSocket,
    private val reader: Reader,
    private val writer: Writer,
) : Closeable {
    fun readEvent(): SourceEvent = SourceEvent.read(reader)

    @Synchronized
    fun sendFrame(streamId: Int, ptsUs: Long, data: ByteArray, length: Int) {
        writer.frame(streamId, ptsUs, data, 0, length)
        writer.flush()
    }

    override fun close() {
        // shutdownInput() wakes a reader blocked in another thread.
        runCatching { socket.shutdownInput() }
        runCatching { socket.shutdownOutput() }
        socket.close()
    }
}

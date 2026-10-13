// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.daemon

import java.io.EOFException
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream

// Daemon protocol, version 2. Mirrors uvcd/src/message.rs, which documents
// the wire format. All integers are little endian.

const val PROTOCOL_VERSION = 2
const val SOCKET_NAME = "uvcamerad"

enum class FormatKind(val wire: Int) {
    MJPEG(0),
    YUYV(1);

    companion object {
        fun fromWire(value: Int) =
            entries.find { it.wire == value } ?: throw IOException("Invalid format kind: $value")
    }
}

/** Frame interval in 100 ns units for [fps]. */
fun fpsToInterval(fps: Int) = 10_000_000 / fps

data class FrameSpec(val width: Int, val height: Int, val intervals: List<Int>)

data class FormatSpec(val kind: FormatKind, val frames: List<FrameSpec>) {
    companion object {
        /** Same as uvcd's built-in defaults. */
        fun defaults(): List<FormatSpec> {
            fun frame(w: Int, h: Int, vararg fps: Int) =
                FrameSpec(w, h, fps.map(::fpsToInterval).sorted())

            return listOf(
                FormatSpec(FormatKind.MJPEG, listOf(
                    frame(1920, 1080, 30, 15),
                    frame(1280, 720, 30, 15),
                    frame(640, 480, 30, 15),
                )),
                FormatSpec(FormatKind.YUYV, listOf(
                    frame(640, 480, 30, 15),
                    frame(1280, 720, 10),
                )),
            )
        }
    }
}

/** `enum usb_device_speed`. */
enum class UsbSpeed(val wire: Int, val label: String) {
    LOW(1, "Low speed"),
    FULL(2, "USB 1.1"),
    HIGH(3, "USB 2.0"),
    WIRELESS(4, "Wireless USB"),
    SUPER(5, "USB 3"),
    SUPER_PLUS(6, "USB 3.1+");

    companion object {
        fun fromWire(value: Int): UsbSpeed? = entries.find { it.wire == value }
    }
}

data class StreamParams(
    val kind: FormatKind,
    val width: Int,
    val height: Int,
    val interval: Int,
    val maxFrameSize: Int,
) {
    val fps: Double get() = 10_000_000.0 / interval
}

data class Status(
    val enabled: Boolean,
    val hostSpeed: UsbSpeed?,
    val streaming: StreamParams?,
    /** Frames sent per second over the daemon's last statistics window. */
    val fps: Double,
    /** New frames received from this app per second over the same window. */
    val sourceFps: Double,
    /** Frames not fully transferred over the last statistics window. */
    val failed: Long,
)

sealed interface Request {
    fun write(out: Writer)

    data object GetStatus : Request {
        override fun write(out: Writer) = out.u8(1)
    }

    data class Enable(val maxburst: Int, val formats: List<FormatSpec>) : Request {
        override fun write(out: Writer) {
            out.u8(2)
            out.u8(maxburst)
            out.u8(formats.size)
            for (format in formats) {
                out.u8(format.kind.wire)
                out.u8(format.frames.size)
                for (frame in format.frames) {
                    out.u16(frame.width)
                    out.u16(frame.height)
                    out.u8(frame.intervals.size)
                    frame.intervals.forEach(out::u32)
                }
            }
        }
    }

    data object Disable : Request {
        override fun write(out: Writer) = out.u8(3)
    }

    /** Turn the connection into the frame source channel. */
    data object AttachSource : Request {
        override fun write(out: Writer) = out.u8(4)
    }
}

sealed interface Response {
    data class Error(val message: String) : Response
    data class StatusResponse(val status: Status) : Response
    data object Ok : Response

    companion object {
        fun read(input: Reader): Response = when (val id = input.u8()) {
            0 -> Error(input.string())
            1 -> {
                val enabled = input.bool()
                val speed = UsbSpeed.fromWire(input.u8())
                val streaming = if (input.bool()) input.streamParams() else null
                val fps = input.u32() / 1000.0
                val sourceFps = input.u32() / 1000.0
                val failed = input.u32()
                StatusResponse(Status(enabled, speed, streaming, fps, sourceFps, failed))
            }
            2 -> Ok
            else -> throw IOException("Unknown response: $id")
        }
    }
}

/** Daemon-to-app message on the source channel. */
sealed interface SourceEvent {
    /** Start producing frames in [params], tagged with [streamId]. */
    data class StreamStart(val streamId: Int, val params: StreamParams) : SourceEvent

    /** The host stopped streaming; release the camera. */
    data class StreamStop(val streamId: Int) : SourceEvent

    companion object {
        fun read(input: Reader): SourceEvent = when (val id = input.u8()) {
            1 -> StreamStart(input.u32().toInt(), input.streamParams())
            2 -> StreamStop(input.u32().toInt())
            else -> throw IOException("Unknown source event: $id")
        }
    }
}

/** Write one frame on the source channel. */
fun Writer.frame(streamId: Int, ptsUs: Long, data: ByteArray, offset: Int = 0, length: Int = data.size) {
    require(length > 0) { "Empty frame" }
    u8(1)
    u32(streamId)
    u64(ptsUs)
    u32(length)
    bytes(data, offset, length)
}

class Reader(private val input: InputStream) {
    fun streamParams() = StreamParams(
        kind = FormatKind.fromWire(u8()),
        width = u16(),
        height = u16(),
        interval = u32().toInt(),
        maxFrameSize = u32().toInt(),
    )

    fun u8(): Int {
        val b = input.read()
        if (b < 0) {
            throw EOFException()
        }
        return b
    }

    fun u16(): Int = u8() or (u8() shl 8)

    /** As a Long, since the value is unsigned. */
    fun u32(): Long = u16().toLong() or (u16().toLong() shl 16)

    fun bool(): Boolean = when (val b = u8()) {
        0 -> false
        1 -> true
        else -> throw IOException("Invalid boolean: $b")
    }

    fun string(): String {
        val buf = ByteArray(u16())
        var offset = 0
        while (offset < buf.size) {
            val n = input.read(buf, offset, buf.size - offset)
            if (n < 0) {
                throw EOFException()
            }
            offset += n
        }
        return buf.decodeToString()
    }
}

class Writer(private val output: OutputStream) {
    fun u8(value: Int) {
        require(value in 0..0xff) { "u8 out of range: $value" }
        output.write(value)
    }

    fun u16(value: Int) {
        require(value in 0..0xffff) { "u16 out of range: $value" }
        output.write(value and 0xff)
        output.write(value ushr 8)
    }

    fun u32(value: Int) {
        u16(value and 0xffff)
        u16(value ushr 16)
    }

    fun u64(value: Long) {
        u32(value.toInt())
        u32((value ushr 32).toInt())
    }

    fun bytes(data: ByteArray, offset: Int, length: Int) = output.write(data, offset, length)

    fun flush() = output.flush()
}

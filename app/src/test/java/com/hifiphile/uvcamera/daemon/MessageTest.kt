// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.daemon

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream

/** The same golden bytes are asserted in uvcd/src/message.rs. */
class MessageTest {
    private fun bytes(vararg values: Int) = ByteArray(values.size) { values[it].toByte() }

    @Test
    fun enableRequestMatchesDaemonEncoding() {
        val out = ByteArrayOutputStream()
        Request.Enable(2, listOf(
            FormatSpec(FormatKind.MJPEG, listOf(FrameSpec(640, 480, listOf(333_333)))),
        )).write(Writer(out))

        assertArrayEquals(
            bytes(2, 2, 1, 0, 1, 0x80, 0x02, 0xe0, 0x01, 1, 0x15, 0x16, 0x05, 0x00),
            out.toByteArray(),
        )
    }

    @Test
    fun statusResponseMatchesDaemonEncoding() {
        val data = bytes(
            1, 1, 5, 1, 1, 0x80, 0x02, 0xe0, 0x01, 0x15, 0x16, 0x05, 0x00,
            0x00, 0x60, 0x09, 0x00, 0x06, 0x75, 0x00, 0x00, 0x3c, 0x73, 0x00, 0x00,
            0x03, 0, 0, 0,
        )

        val response = Response.read(Reader(ByteArrayInputStream(data)))

        assertEquals(
            Response.StatusResponse(Status(
                enabled = true,
                hostSpeed = UsbSpeed.SUPER,
                streaming = StreamParams(FormatKind.YUYV, 640, 480, 333_333, 614_400),
                fps = 29.958,
                sourceFps = 29.5,
                failed = 3,
            )),
            response,
        )
    }

    @Test
    fun frameMatchesDaemonEncoding() {
        val out = ByteArrayOutputStream()
        Writer(out).frame(7, 0x0102_0304_0506_0708L, bytes(0xff, 0xd8))

        assertArrayEquals(
            bytes(1, 7, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2, 1, 2, 0, 0, 0, 0xff, 0xd8),
            out.toByteArray(),
        )
    }

    @Test
    fun sourceEvents() {
        val start = bytes(1, 7, 0, 0, 0, 0, 0x80, 0x07, 0x38, 0x04, 0x15, 0x16, 0x05, 0x00, 0, 0x48, 0x3f, 0)
        assertEquals(
            SourceEvent.StreamStart(7, StreamParams(FormatKind.MJPEG, 1920, 1080, 333_333, 4_147_200)),
            SourceEvent.read(Reader(ByteArrayInputStream(start))),
        )
        assertEquals(
            SourceEvent.StreamStop(7),
            SourceEvent.read(Reader(ByteArrayInputStream(bytes(2, 7, 0, 0, 0)))),
        )
        // Same bytes as uvcd's message.rs.
        assertEquals(
            SourceEvent.Control(CONTROL_BRIGHTNESS, 200),
            SourceEvent.read(Reader(ByteArrayInputStream(bytes(3, 1, 200, 0, 0, 0)))),
        )
    }

    @Test
    fun errorResponse() {
        val data = bytes(0, 4, 0, 'n'.code, 'o'.code, 'p'.code, 'e'.code)
        assertEquals(Response.Error("nope"), Response.read(Reader(ByteArrayInputStream(data))))
    }
}

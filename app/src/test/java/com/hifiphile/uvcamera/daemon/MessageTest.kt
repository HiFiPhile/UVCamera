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
            0x00, 0x60, 0x09, 0x00, 0x06, 0x75, 0x00, 0x00, 0x03, 0, 0, 0,
        )

        val response = Response.read(Reader(ByteArrayInputStream(data)))

        assertEquals(
            Response.StatusResponse(Status(
                enabled = true,
                hostSpeed = UsbSpeed.SUPER,
                streaming = StreamParams(FormatKind.YUYV, 640, 480, 333_333, 614_400),
                fps = 29.958,
                failed = 3,
            )),
            response,
        )
    }

    @Test
    fun errorResponse() {
        val data = bytes(0, 4, 0, 'n'.code, 'o'.code, 'p'.code, 'e'.code)
        assertEquals(Response.Error("nope"), Response.read(Reader(ByteArrayInputStream(data))))
    }
}

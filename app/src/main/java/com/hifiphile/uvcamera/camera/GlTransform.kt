// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.camera

import android.graphics.SurfaceTexture
import android.opengl.EGL14
import android.opengl.EGLConfig
import android.opengl.EGLContext
import android.opengl.EGLDisplay
import android.opengl.EGLExt
import android.opengl.EGLSurface
import android.opengl.GLES11Ext
import android.opengl.GLES20
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import android.util.Log
import android.view.Surface
import java.io.Closeable
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.FloatBuffer
import java.util.concurrent.CountDownLatch

/**
 * Texture coordinates of the output's corners, top-left, top-right,
 * bottom-left, bottom-right, in a [srcWidth]x[srcHeight] camera buffer as
 * stored (s right, t down from the first row): the same mapping as
 * [YuvTransform] (rotate clockwise, mirror, crop the center), in continuous
 * coordinates.
 */
fun transformTexCoords(srcWidth: Int, srcHeight: Int, outWidth: Int, outHeight: Int, transform: Transform): FloatArray {
    val w = srcWidth.toDouble()
    val h = srcHeight.toDouble()
    val (rw, rh) = transform.rotatedSize(srcWidth, srcHeight)
    // Even crop offsets, as YuvTransform needs for chroma: the same framing.
    val cx = ((rw - outWidth) / 2 and 1.inv()).toDouble()
    val cy = ((rh - outHeight) / 2 and 1.inv()).toDouble()

    val coords = FloatArray(8)
    for ((i, corner) in listOf(0 to 0, 1 to 0, 0 to 1, 1 to 1).withIndex()) {
        val xc = corner.first * outWidth + cx
        val yc = corner.second * outHeight + cy
        val xr = if (transform.mirrorH) rw - xc else xc
        val yr = if (transform.mirrorV) rh - yc else yc
        val (sx, sy) = when (transform.rotation) {
            Rotation.R0 -> xr to yr
            Rotation.R90 -> yr to h - xr
            Rotation.R180 -> w - xr to h - yr
            Rotation.R270 -> w - yr to xr
        }
        coords[i * 2] = (sx / w).toFloat()
        coords[i * 2 + 1] = (sy / h).toFloat()
    }
    return coords
}

/**
 * Draws camera frames into [target] (an encoder's input surface) with the
 * GPU, rotated, mirrored and cropped by a [Transform]. The camera renders
 * into [cameraSurface], a [srcWidth]x[srcHeight] SurfaceTexture.
 *
 * The camera buffer is sampled as stored, ignoring the SurfaceTexture's
 * transform matrix: the camera service sets display rotation and front
 * camera mirroring there, while [Transform] works on the sensor image, like
 * the YUV path.
 */
class GlTransform(
    private val srcWidth: Int,
    private val srcHeight: Int,
    private val outWidth: Int,
    private val outHeight: Int,
    transform: Transform,
    target: Surface,
    /** Camera timestamps are in the elapsedRealtime (boot time) clock. */
    private val realtimeTimestamps: Boolean,
) : Closeable {
    companion object {
        private const val TAG = "GlTransform"

        private const val VERTEX_SHADER = """
            attribute vec2 aPosition;
            attribute vec2 aTexCoord;
            varying vec2 vTexCoord;
            void main() {
                gl_Position = vec4(aPosition, 0.0, 1.0);
                vTexCoord = aTexCoord;
            }
        """

        private const val FRAGMENT_SHADER = """
            #extension GL_OES_EGL_image_external : require
            precision mediump float;
            uniform samplerExternalOES uTexture;
            varying vec2 vTexCoord;
            void main() {
                gl_FragColor = texture2D(uTexture, vTexCoord);
            }
        """

        /** Clip space corners in the order of [transformTexCoords]: top row first. */
        private val POSITIONS = floatArrayOf(-1f, 1f, 1f, 1f, -1f, -1f, 1f, -1f)

        private fun floats(values: FloatArray): FloatBuffer =
            ByteBuffer.allocateDirect(values.size * 4).order(ByteOrder.nativeOrder()).asFloatBuffer()
                .apply { put(values).position(0) }
    }

    private val thread = HandlerThread("gl-transform").apply { start() }
    private val handler = Handler(thread.looper)

    private lateinit var display: EGLDisplay
    private lateinit var context: EGLContext
    private lateinit var eglSurface: EGLSurface
    private var program = 0
    private var texture = 0
    private lateinit var surfaceTexture: SurfaceTexture

    /** Where the camera renders. */
    lateinit var cameraSurface: Surface
        private set

    private val positions = floats(POSITIONS)

    @Volatile
    private var texCoords = floats(transformTexCoords(srcWidth, srcHeight, outWidth, outHeight, transform))

    @Volatile
    private var closed = false

    init {
        val ready = CountDownLatch(1)
        var error: Throwable? = null
        handler.post {
            try {
                setUp(target)
            } catch (e: Throwable) {
                error = e
            }
            ready.countDown()
        }
        ready.await()
        error?.let {
            thread.quitSafely()
            throw IllegalStateException("Failed to set up OpenGL", it)
        }
    }

    private fun check(what: String) {
        val error = EGL14.eglGetError()
        if (error != EGL14.EGL_SUCCESS) {
            throw IllegalStateException("$what failed: EGL error 0x${Integer.toHexString(error)}")
        }
    }

    private fun setUp(target: Surface) {
        display = EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY)
        val version = IntArray(2)
        EGL14.eglInitialize(display, version, 0, version, 1) || error("eglInitialize failed")

        val configs = arrayOfNulls<EGLConfig>(1)
        val count = IntArray(1)
        EGL14.eglChooseConfig(display, intArrayOf(
            EGL14.EGL_RED_SIZE, 8,
            EGL14.EGL_GREEN_SIZE, 8,
            EGL14.EGL_BLUE_SIZE, 8,
            EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT,
            EGLExt.EGL_RECORDABLE_ANDROID, 1,
            EGL14.EGL_NONE,
        ), 0, configs, 0, 1, count, 0)
        check("eglChooseConfig")
        val config = configs[0] ?: error("No recordable EGL config")

        context = EGL14.eglCreateContext(display, config, EGL14.EGL_NO_CONTEXT,
            intArrayOf(EGL14.EGL_CONTEXT_CLIENT_VERSION, 2, EGL14.EGL_NONE), 0)
        check("eglCreateContext")
        eglSurface = EGL14.eglCreateWindowSurface(display, config, target, intArrayOf(EGL14.EGL_NONE), 0)
        check("eglCreateWindowSurface")
        EGL14.eglMakeCurrent(display, eglSurface, eglSurface, context)
        check("eglMakeCurrent")

        program = linkProgram()
        val textures = IntArray(1)
        GLES20.glGenTextures(1, textures, 0)
        texture = textures[0]
        GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, texture)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MIN_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MAG_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_WRAP_S, GLES20.GL_CLAMP_TO_EDGE)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_WRAP_T, GLES20.GL_CLAMP_TO_EDGE)

        surfaceTexture = SurfaceTexture(texture).apply {
            setDefaultBufferSize(srcWidth, srcHeight)
            setOnFrameAvailableListener({ draw() }, handler)
        }
        cameraSurface = Surface(surfaceTexture)
        Log.i(TAG, "Rendering ${srcWidth}x$srcHeight -> ${outWidth}x$outHeight, GL ${version[0]}.${version[1]}")
    }

    private fun compile(type: Int, source: String): Int {
        val shader = GLES20.glCreateShader(type)
        GLES20.glShaderSource(shader, source)
        GLES20.glCompileShader(shader)
        val status = IntArray(1)
        GLES20.glGetShaderiv(shader, GLES20.GL_COMPILE_STATUS, status, 0)
        if (status[0] == 0) {
            error("Shader compilation failed: ${GLES20.glGetShaderInfoLog(shader)}")
        }
        return shader
    }

    private fun linkProgram(): Int {
        val program = GLES20.glCreateProgram()
        GLES20.glAttachShader(program, compile(GLES20.GL_VERTEX_SHADER, VERTEX_SHADER))
        GLES20.glAttachShader(program, compile(GLES20.GL_FRAGMENT_SHADER, FRAGMENT_SHADER))
        GLES20.glLinkProgram(program)
        val status = IntArray(1)
        GLES20.glGetProgramiv(program, GLES20.GL_LINK_STATUS, status, 0)
        if (status[0] == 0) {
            error("Program link failed: ${GLES20.glGetProgramInfoLog(program)}")
        }
        return program
    }

    /** Use [transform] from the next frame on. The source size must not change. */
    fun setTransform(transform: Transform) {
        texCoords = floats(transformTexCoords(srcWidth, srcHeight, outWidth, outHeight, transform))
    }

    private fun draw() {
        if (closed) {
            return
        }
        surfaceTexture.updateTexImage()
        // Capture time in CLOCK_MONOTONIC, as the protocol expects; the
        // encoder passes it through as the presentation time.
        val timestamp = surfaceTexture.timestamp
        val ptsNs = if (realtimeTimestamps) {
            System.nanoTime() - (SystemClock.elapsedRealtimeNanos() - timestamp)
        } else {
            timestamp
        }

        GLES20.glViewport(0, 0, outWidth, outHeight)
        GLES20.glUseProgram(program)
        GLES20.glActiveTexture(GLES20.GL_TEXTURE0)
        GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, texture)
        GLES20.glUniform1i(GLES20.glGetUniformLocation(program, "uTexture"), 0)

        val position = GLES20.glGetAttribLocation(program, "aPosition")
        GLES20.glEnableVertexAttribArray(position)
        GLES20.glVertexAttribPointer(position, 2, GLES20.GL_FLOAT, false, 0, positions)
        val texCoord = GLES20.glGetAttribLocation(program, "aTexCoord")
        GLES20.glEnableVertexAttribArray(texCoord)
        GLES20.glVertexAttribPointer(texCoord, 2, GLES20.GL_FLOAT, false, 0, texCoords)
        GLES20.glDrawArrays(GLES20.GL_TRIANGLE_STRIP, 0, 4)

        EGLExt.eglPresentationTimeANDROID(display, eglSurface, ptsNs)
        EGL14.eglSwapBuffers(display, eglSurface)
    }

    override fun close() {
        closed = true
        val done = CountDownLatch(1)
        handler.post {
            runCatching {
                cameraSurface.release()
                surfaceTexture.release()
                GLES20.glDeleteTextures(1, intArrayOf(texture), 0)
                GLES20.glDeleteProgram(program)
                EGL14.eglMakeCurrent(display, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_CONTEXT)
                EGL14.eglDestroySurface(display, eglSurface)
                // Not eglTerminate: the display is shared with the UI's renderer.
                EGL14.eglDestroyContext(display, context)
            }.onFailure { Log.w(TAG, "Failed to release OpenGL", it) }
            done.countDown()
        }
        done.await()
        thread.quitSafely()
    }
}

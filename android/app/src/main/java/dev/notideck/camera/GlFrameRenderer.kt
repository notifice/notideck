// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

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
import android.os.Looper
import android.util.Log
import android.view.Surface
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * Camera -> SurfaceTexture -> GL -> encoder input Surface. The GL pass exists to rotate the sensor
 * image upright for the current display orientation and to mirror it on request; the encoder has
 * no rotation metadata path. Everything GL runs on one private thread.
 *
 * Round 28: while [preview] has a target, every frame is also drawn to that surface (a second EGL window
 * surface, centre-cropped to its aspect), after the encoder. Preview failures drop the preview and never
 * touch the encoder path. [encoderSurface] null = preview only: a 1x1 pbuffer (or no surface, with
 * EGL_KHR_surfaceless_context) keeps the context current while no preview surface is attached.
 */
class GlFrameRenderer(
    private val encoderSurface: Surface?,
    private val width: Int,
    private val height: Int,
    private val preview: PreviewOutput? = null
) {
    companion object {
        const val TAG = "GlFrameRenderer"
        private const val EGL_RECORDABLE_ANDROID = 0x3142
        private const val VERTEX = """
            uniform mat4 uTex; uniform mat4 uRot;
            attribute vec4 aPos; attribute vec2 aTc; varying vec2 vTc;
            void main() {
              gl_Position = aPos;
              vec2 c = (uRot * vec4(aTc - 0.5, 0.0, 1.0)).xy + 0.5;
              vTc = (uTex * vec4(c, 0.0, 1.0)).xy;
            }"""
        private const val FRAGMENT = """
            #extension GL_OES_EGL_image_external : require
            precision mediump float;
            varying vec2 vTc; uniform samplerExternalOES uSampler;
            void main() { gl_FragColor = texture2D(uSampler, vTc); }"""
        private val QUAD = floatArrayOf(
            -1f, -1f, 0f, 0f,
            1f, -1f, 1f, 0f,
            -1f, 1f, 0f, 1f,
            1f, 1f, 1f, 1f
        )
    }

    /** Clockwise degrees the sensor image must be turned to be upright (0/90/180/270). */
    @Volatile var rotationDeg = 0
    @Volatile var mirror = false

    private val thread = HandlerThread("nd-gl").apply { start() }
    private val handler = Handler(thread.looper)
    private var display: EGLDisplay = EGL14.EGL_NO_DISPLAY
    private var context: EGLContext = EGL14.EGL_NO_CONTEXT
    private var config: EGLConfig? = null
    /** Encoder window surface, or the preview-only pbuffer, or EGL_NO_SURFACE (surfaceless). */
    private var baseSurface: EGLSurface = EGL14.EGL_NO_SURFACE
    private var current: EGLSurface = EGL14.EGL_NO_SURFACE
    private var program = 0
    private var texId = 0
    private var uTex = 0
    private var uRot = 0
    private var aPos = 0
    private var aTc = 0
    private var surfaceTexture: SurfaceTexture? = null
    private val quad = ByteBuffer.allocateDirect(QUAD.size * 4).order(ByteOrder.nativeOrder())
        .asFloatBuffer().apply { put(QUAD); position(0) }
    private val texMatrix = FloatArray(16)
    private val rotMatrix = FloatArray(16)
    @Volatile private var released = false
    @Volatile var framesRendered = 0L
        private set
    /** SurfaceTexture frames delivered by the camera (before drawing). */
    @Volatile var framesIn = 0L
        private set
    private var loggedStrip = false

    // preview (GL thread only)
    private var previewTarget: PreviewTarget? = null
    private var previewEgl: EGLSurface = EGL14.EGL_NO_SURFACE
    private var previewDrawn = false
    /** Dropped after an error; not retried until the controller sets a different target. */
    private var failedTarget: PreviewTarget? = null
    private val previewMatrix = FloatArray(16)
    private val dims = IntArray(1)

    /** Creates the GL state and returns the SurfaceTexture the camera should write to. */
    fun start(): SurfaceTexture {
        val latch = CountDownLatch(1)
        var error: Throwable? = null
        handler.post {
            try { setup() } catch (e: Throwable) { error = e } finally { latch.countDown() }
        }
        if (!latch.await(5, TimeUnit.SECONDS)) throw IllegalStateException("GL setup timed out")
        error?.let { release(); throw it }
        return surfaceTexture!!
    }

    private fun chooseConfig(): EGLConfig {
        fun pick(recordable: Boolean, pbuffer: Boolean): EGLConfig? {
            val a = mutableListOf(
                EGL14.EGL_RED_SIZE, 8, EGL14.EGL_GREEN_SIZE, 8, EGL14.EGL_BLUE_SIZE, 8, EGL14.EGL_ALPHA_SIZE, 8,
                EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT
            )
            if (recordable) a += listOf(EGL_RECORDABLE_ANDROID, 1)
            if (pbuffer) a += listOf(EGL14.EGL_SURFACE_TYPE, EGL14.EGL_WINDOW_BIT or EGL14.EGL_PBUFFER_BIT)
            a += EGL14.EGL_NONE
            val configs = arrayOfNulls<EGLConfig>(1)
            val n = IntArray(1)
            return if (EGL14.eglChooseConfig(display, a.toIntArray(), 0, configs, 0, 1, n, 0) && n[0] > 0) configs[0] else null
        }
        // Encoding keeps the exact config it always used; preview only wants a pbuffer-capable one if it exists.
        val c = if (encoderSurface != null) pick(recordable = true, pbuffer = false)
        else pick(recordable = false, pbuffer = true) ?: pick(recordable = false, pbuffer = false)
        return checkNotNull(c) { "eglChooseConfig" }
    }

    private fun setup() {
        display = EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY)
        val ver = IntArray(2)
        check(EGL14.eglInitialize(display, ver, 0, ver, 1)) { "eglInitialize" }
        val cfg = chooseConfig()
        config = cfg
        context = EGL14.eglCreateContext(display, cfg, EGL14.EGL_NO_CONTEXT,
            intArrayOf(EGL14.EGL_CONTEXT_CLIENT_VERSION, 2, EGL14.EGL_NONE), 0)
        check(context != EGL14.EGL_NO_CONTEXT) { "eglCreateContext" }
        if (encoderSurface != null) {
            baseSurface = EGL14.eglCreateWindowSurface(display, cfg, encoderSurface, intArrayOf(EGL14.EGL_NONE), 0)
            check(baseSurface != EGL14.EGL_NO_SURFACE) { "eglCreateWindowSurface" }
        } else {
            baseSurface = EGL14.eglCreatePbufferSurface(display, cfg, intArrayOf(EGL14.EGL_WIDTH, 1, EGL14.EGL_HEIGHT, 1, EGL14.EGL_NONE), 0)
            if (baseSurface == EGL14.EGL_NO_SURFACE) Log.i(TAG, "no pbuffer (0x%x), going surfaceless".format(EGL14.eglGetError()))
        }
        check(EGL14.eglMakeCurrent(display, baseSurface, baseSurface, context)) { "eglMakeCurrent 0x%x".format(EGL14.eglGetError()) }
        current = baseSurface

        program = link(VERTEX, FRAGMENT)
        aPos = GLES20.glGetAttribLocation(program, "aPos")
        aTc = GLES20.glGetAttribLocation(program, "aTc")
        uTex = GLES20.glGetUniformLocation(program, "uTex")
        uRot = GLES20.glGetUniformLocation(program, "uRot")
        val t = IntArray(1)
        GLES20.glGenTextures(1, t, 0)
        texId = t[0]
        GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, texId)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MIN_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MAG_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_WRAP_S, GLES20.GL_CLAMP_TO_EDGE)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_WRAP_T, GLES20.GL_CLAMP_TO_EDGE)
        val st = SurfaceTexture(texId)
        st.setDefaultBufferSize(width, height)
        st.setOnFrameAvailableListener({ framesIn++; draw() }, handler)
        surfaceTexture = st
        preview?.bind(this)
        applyPreviewSafely()
    }

    private fun makeCurrent(s: EGLSurface): Boolean {
        if (s == current) return true
        if (!EGL14.eglMakeCurrent(display, s, s, context)) return false
        current = s
        return true
    }

    private fun draw() {
        if (released) return
        val st = surfaceTexture ?: return
        try {
            // Any surface (or none) may be current here; updateTexImage only needs the context.
            st.updateTexImage()
            st.getTransformMatrix(texMatrix)
            // Some HALs (Xperia XZ, Android 8) tag camera buffers with a 90° turn; orientation is ours to apply.
            if (CameraGeometry.stripTransformRotation(texMatrix) && !loggedStrip) {
                loggedStrip = true
                Log.i(TAG, "SurfaceTexture transform carried a rotation; using the plain y-flip")
            }
        } catch (e: Throwable) {
            if (!released) Log.w(TAG, "draw failed: $e")
            return
        }
        applyPreviewSafely()
        if (encoderSurface != null) {
            try {
                check(makeCurrent(baseSurface)) { "eglMakeCurrent(encoder) 0x%x".format(EGL14.eglGetError()) }
                // Rotation, mirror and cover-crop in one aspect-correct matrix (90/270 swap the aspect).
                CameraGeometry.coverMatrix4(rotationDeg, mirror, width, height, width, height, rotMatrix)
                drawQuad(width, height, rotMatrix)
                EGLExt.eglPresentationTimeANDROID(display, baseSurface, st.timestamp)
                EGL14.eglSwapBuffers(display, baseSurface)
                framesRendered++
            } catch (e: Throwable) {
                if (!released) Log.w(TAG, "draw failed: $e")
            }
        }
        val t = previewTarget ?: return
        try {
            drawPreview(t)
        } catch (e: Throwable) {
            failPreview(t, "preview: ${e.message ?: e.javaClass.simpleName}")
        }
    }

    private fun drawQuad(w: Int, h: Int, rot: FloatArray) {
        GLES20.glViewport(0, 0, w, h)
        GLES20.glUseProgram(program)
        GLES20.glActiveTexture(GLES20.GL_TEXTURE0)
        GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, texId)
        GLES20.glUniformMatrix4fv(uTex, 1, false, texMatrix, 0)
        GLES20.glUniformMatrix4fv(uRot, 1, false, rot, 0)
        quad.position(0)
        GLES20.glVertexAttribPointer(aPos, 2, GLES20.GL_FLOAT, false, 16, quad)
        GLES20.glEnableVertexAttribArray(aPos)
        quad.position(2)
        GLES20.glVertexAttribPointer(aTc, 2, GLES20.GL_FLOAT, false, 16, quad)
        GLES20.glEnableVertexAttribArray(aTc)
        GLES20.glDrawArrays(GLES20.GL_TRIANGLE_STRIP, 0, 4)
    }

    // ------------------------------------------------------------------ preview (GL thread)

    private fun drawPreview(t: PreviewTarget) {
        val s = previewEgl
        if (s == EGL14.EGL_NO_SURFACE) return
        if (!makeCurrent(s)) return failPreview(t, "preview: eglMakeCurrent 0x%x".format(EGL14.eglGetError()))
        // The real buffer size wins over the size the view reported (they normally match).
        val w = if (EGL14.eglQuerySurface(display, s, EGL14.EGL_WIDTH, dims, 0) && dims[0] > 0) dims[0] else t.width
        val h = if (EGL14.eglQuerySurface(display, s, EGL14.EGL_HEIGHT, dims, 0) && dims[0] > 0) dims[0] else t.height
        CameraGeometry.previewMatrix4(rotationDeg, mirror, width, height, w, h, previewMatrix)
        drawQuad(w, h, previewMatrix)
        if (!EGL14.eglSwapBuffers(display, s)) return failPreview(t, "preview: eglSwapBuffers 0x%x".format(EGL14.eglGetError()))
        if (encoderSurface == null) framesRendered++
        if (!previewDrawn) {
            previewDrawn = true
            preview?.active(t, true)
        }
    }

    private fun applyPreviewSafely() {
        try {
            applyPreview()
        } catch (e: Throwable) {
            val t = previewTarget
            if (t != null) failPreview(t, "preview: ${e.message ?: e.javaClass.simpleName}") else Log.w(TAG, "preview: $e")
        }
    }

    /** Brings the preview EGL surface in line with [PreviewOutput.target]. */
    private fun applyPreview() {
        val t = preview?.target
        if (t === previewTarget) return
        if (t != null && t === failedTarget) return
        val old = previewTarget
        if (old != null && t != null && old.surface === t.surface && previewEgl != EGL14.EGL_NO_SURFACE) {
            // Same surface, new size: the window surface follows the buffer size by itself.
            previewTarget = t
            previewDrawn = false
            return
        }
        dropPreview()
        if (t == null) return
        previewTarget = t
        val s = try {
            EGL14.eglCreateWindowSurface(display, config, t.surface, intArrayOf(EGL14.EGL_NONE), 0)
        } catch (e: Throwable) {
            return failPreview(t, "preview surface: ${e.message ?: e.javaClass.simpleName}")
        }
        if (s == null || s == EGL14.EGL_NO_SURFACE) {
            return failPreview(t, "preview surface: EGL error 0x%x".format(EGL14.eglGetError()))
        }
        previewEgl = s
        // Non-blocking swaps (async buffer queue): a slow view must never hold up the encoder.
        if (makeCurrent(s)) EGL14.eglSwapInterval(display, 0)
    }

    private fun dropPreview() {
        val t = previewTarget
        val s = previewEgl
        previewTarget = null
        previewEgl = EGL14.EGL_NO_SURFACE
        if (s != EGL14.EGL_NO_SURFACE) {
            // A current surface is only destroyed once released, so step back to the base surface first.
            if (current == s && !makeCurrent(baseSurface)) Log.w(TAG, "preview: cannot release the current surface")
            try { EGL14.eglDestroySurface(display, s) } catch (e: Throwable) { Log.w(TAG, "preview destroy: $e") }
        }
        if (t != null && previewDrawn) preview?.active(t, false)
        previewDrawn = false
    }

    private fun failPreview(t: PreviewTarget, message: String) {
        Log.w(TAG, message)
        dropPreview()
        failedTarget = t
        preview?.error(t, message)
    }

    /** Applies the current [PreviewOutput.target] now and waits briefly for it; safe from any thread. */
    internal fun syncPreview() {
        if (released) return
        if (Looper.myLooper() === handler.looper) return applyPreviewSafely()
        val latch = CountDownLatch(1)
        val posted = handler.post {
            try { if (!released && display != EGL14.EGL_NO_DISPLAY) applyPreviewSafely() } finally { latch.countDown() }
        }
        if (posted) latch.await(300, TimeUnit.MILLISECONDS)
    }

    fun release() {
        if (released) return
        released = true
        val latch = CountDownLatch(1)
        handler.post {
            try {
                preview?.unbind(this)
                surfaceTexture?.setOnFrameAvailableListener(null)
                surfaceTexture?.release()
                if (display != EGL14.EGL_NO_DISPLAY) {
                    dropPreview()
                    EGL14.eglMakeCurrent(display, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_CONTEXT)
                    if (baseSurface != EGL14.EGL_NO_SURFACE) EGL14.eglDestroySurface(display, baseSurface)
                    if (context != EGL14.EGL_NO_CONTEXT) EGL14.eglDestroyContext(display, context)
                    EGL14.eglTerminate(display)
                }
            } catch (e: Throwable) {
                Log.w(TAG, "release: $e")
            } finally {
                display = EGL14.EGL_NO_DISPLAY
                latch.countDown()
            }
        }
        latch.await(2, TimeUnit.SECONDS)
        thread.quitSafely()
    }

    private fun link(vs: String, fs: String): Int {
        fun compile(type: Int, src: String): Int {
            val s = GLES20.glCreateShader(type)
            GLES20.glShaderSource(s, src)
            GLES20.glCompileShader(s)
            val ok = IntArray(1)
            GLES20.glGetShaderiv(s, GLES20.GL_COMPILE_STATUS, ok, 0)
            check(ok[0] != 0) { "shader: ${GLES20.glGetShaderInfoLog(s)}" }
            return s
        }
        val p = GLES20.glCreateProgram()
        GLES20.glAttachShader(p, compile(GLES20.GL_VERTEX_SHADER, vs))
        GLES20.glAttachShader(p, compile(GLES20.GL_FRAGMENT_SHADER, fs))
        GLES20.glLinkProgram(p)
        val ok = IntArray(1)
        GLES20.glGetProgramiv(p, GLES20.GL_LINK_STATUS, ok, 0)
        check(ok[0] != 0) { "program: ${GLES20.glGetProgramInfoLog(p)}" }
        return p
    }
}

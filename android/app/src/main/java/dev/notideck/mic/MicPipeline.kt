// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.mic

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.media.audiofx.AcousticEchoCanceler
import android.media.audiofx.AudioEffect
import android.media.audiofx.AutomaticGainControl
import android.media.audiofx.NoiseSuppressor
import android.os.Process
import android.os.SystemClock
import android.util.Log
import dev.notideck.camera.MediaPacket
import dev.notideck.net.MediaConnection
import dev.notideck.proto.v1.MediaKind
import java.io.IOException
import java.net.Socket
import java.util.concurrent.atomic.AtomicBoolean

interface MicPipelineListener {
    /** First frame written to the host. */
    fun onStreaming()
    /** Fatal: the pipeline has stopped itself. */
    fun onFailure(message: String)
}

/**
 * AudioRecord (mono PCM16) -> drop-oldest queue -> audio media TLS connection.
 * Threads: capture (nd-mic-read), socket writer (nd-mic-writer). [openMediaSocket] must return a
 * connected, handshaken (TLS) socket.
 */
class MicPipeline(
    private val sampleRate: Int,
    private val frameMs: Int,
    private val voiceProcessing: Boolean,
    private val sessionId: String,
    private val openMediaSocket: () -> Socket,
    private val listener: MicPipelineListener,
    private val queue: AudioDropQueue = AudioDropQueue()
) {
    companion object {
        const val TAG = "MicPipeline"
    }

    private val stopped = AtomicBoolean(false)
    private val failed = AtomicBoolean(false)
    private var record: AudioRecord? = null
    private val effects = mutableListOf<AudioEffect>()
    private var conn: MediaConnection? = null
    @Volatile private var socket: Socket? = null
    private var readThread: Thread? = null
    private var writerThread: Thread? = null

    /** The rate actually captured; valid after [start]. */
    @Volatile var actualRate = sampleRate
        private set

    /** Opens the recorder and the media connection; throws (after cleaning up) on failure. */
    fun start() {
        try {
            openRecorder()
            val c = MediaConnection.attach(openMediaSocket, sessionId, MediaKind.MEDIA_KIND_AUDIO) { socket = it }
            conn = c
            val r = record!!
            r.startRecording()
            if (r.recordingState != AudioRecord.RECORDSTATE_RECORDING) throw IOException("microphone is busy")
            val out = c.out
            writerThread = Thread({ writerLoop(out) }, "nd-mic-writer").also { it.start() }
            readThread = Thread({ readLoop(r) }, "nd-mic-read").also { it.start() }
        } catch (e: Throwable) {
            stop()
            throw e
        }
    }

    @SuppressLint("MissingPermission")
    private fun openRecorder() {
        val source = if (voiceProcessing) MediaRecorder.AudioSource.VOICE_COMMUNICATION else MediaRecorder.AudioSource.MIC
        var last: Throwable? = null
        for (rate in MicAudio.rateCandidates(sampleRate)) {
            val minBuf = AudioRecord.getMinBufferSize(rate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
            if (minBuf <= 0) continue
            val size = maxOf(minBuf * 2, MicAudio.frameBytes(rate, frameMs) * 4)
            try {
                val r = AudioRecord(source, rate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT, size)
                if (r.state != AudioRecord.STATE_INITIALIZED) {
                    r.release()
                    continue
                }
                record = r
                actualRate = rate
                if (voiceProcessing) attachEffects(r.audioSessionId)
                return
            } catch (e: Throwable) {
                last = e
                Log.w(TAG, "AudioRecord at $rate Hz failed: $e")
            }
        }
        throw IOException("microphone unavailable" + (last?.message?.let { ": $it" } ?: ""))
    }

    private fun attachEffects(session: Int) {
        fun add(available: Boolean, create: () -> AudioEffect?) {
            if (!available) return
            try { create()?.let { it.enabled = true; effects += it } } catch (e: Throwable) { Log.w(TAG, "audio effect: $e") }
        }
        add(NoiseSuppressor.isAvailable()) { NoiseSuppressor.create(session) }
        add(AutomaticGainControl.isAvailable()) { AutomaticGainControl.create(session) }
        add(AcousticEchoCanceler.isAvailable()) { AcousticEchoCanceler.create(session) }
    }

    fun stop() {
        if (!stopped.compareAndSet(false, true)) return
        queue.close()
        try { record?.stop() } catch (_: Exception) {}
        conn?.close() ?: try { socket?.close() } catch (_: Exception) {}
        readThread?.let { if (it !== Thread.currentThread()) it.join(1000) }
        writerThread?.let { if (it !== Thread.currentThread()) it.join(1000) }
        for (e in effects) try { e.release() } catch (_: Exception) {}
        effects.clear()
        try { record?.release() } catch (_: Exception) {}
        record = null
    }

    private fun fail(msg: String) {
        if (stopped.get() || !failed.compareAndSet(false, true)) return
        Log.w(TAG, "mic failure: $msg")
        listener.onFailure(msg)
    }

    private fun readLoop(r: AudioRecord) {
        try { Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO) } catch (_: Throwable) {}
        val n = MicAudio.frameSamples(actualRate, frameMs)
        val buf = ShortArray(n)
        val pts = PtsTracker(actualRate)
        try {
            while (!stopped.get()) {
                var got = 0
                while (got < n) {
                    val k = r.read(buf, got, n - got)
                    if (k < 0) {
                        if (!stopped.get()) fail("microphone read failed ($k)")
                        return
                    }
                    got += k
                }
                val endUs = SystemClock.elapsedRealtimeNanos() / 1000
                queue.offer(AudioFrame(pts.next(endUs, n), MicAudio.packPcm16(buf, n), MicAudio.samplesToUs(n.toLong(), actualRate)))
            }
        } catch (e: Throwable) {
            if (!stopped.get()) fail("microphone read failed")
        }
    }

    private fun writerLoop(out: java.io.DataOutputStream) {
        var announced = false
        try {
            while (true) {
                val f = queue.take() ?: break
                out.write(MediaPacket.header(f.ptsUs, config = false, keyFrame = false, len = f.data.size))
                out.write(f.data)
                out.flush()
                if (!announced) { announced = true; listener.onStreaming() }
            }
        } catch (e: Throwable) {
            if (!stopped.get()) fail("media connection lost")
        }
    }
}

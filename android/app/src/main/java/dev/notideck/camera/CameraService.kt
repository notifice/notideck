// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log
import androidx.core.content.ContextCompat
import dev.notideck.AppRuntime
import dev.notideck.MainActivity

/**
 * Foreground service for the whole control session (type connectedDevice, plus camera when the
 * CAMERA permission is granted, and microphone when RECORD_AUDIO is granted).
 *
 * Android 12+ refuses to start a camera-type foreground service from the background, so the
 * session service is started while the app is visible (when the control session reaches Welcome)
 * and keeps the camera type for the session. A later CameraStart then works with the screen off or
 * the app in the background. The pipeline itself lives in [CameraController]; this holds the
 * notification and, while streaming, a partial wake lock.
 */
class CameraService : Service() {
    companion object {
        const val TAG = "CameraService"
        const val CHANNEL = "camera"
        const val NOTIFICATION_ID = 41
        const val ACTION_STOP = "dev.notideck.camera.STOP"            // "Disconnect" button
        const val ACTION_SESSION = "dev.notideck.camera.SESSION"      // session is live (extra host)
        const val ACTION_END_SESSION = "dev.notideck.camera.END_SESSION"
        const val ACTION_CAMERA_ON = "dev.notideck.camera.CAMERA_ON"
        const val ACTION_CAMERA_OFF = "dev.notideck.camera.CAMERA_OFF"
        const val ACTION_MIC_ON = "dev.notideck.camera.MIC_ON"
        const val ACTION_MIC_OFF = "dev.notideck.camera.MIC_OFF"
        const val ACTION_REFRESH = "dev.notideck.camera.REFRESH"      // re-enter foreground (new permission)
        const val EXTRA_HOST = "host"

        /** Service is alive (foreground). */
        @Volatile var running = false
        /** The session notification is up, so a connection is being held. */
        @Volatile var sessionUp = false
        /** The service entered foreground with the camera type, so the camera may open from the background. */
        @Volatile var holdsCamera = false
        /**
         * The service entered foreground with the microphone type (Android 11+), so the microphone may
         * open from the background. Always true below API 30, where no such rule exists.
         */
        @Volatile var holdsMic = false
        /** An activity is started; set by MainActivity. */
        @Volatile var appVisible = false

        fun startSession(context: Context, host: String) {
            try {
                ContextCompat.startForegroundService(
                    context, Intent(context, CameraService::class.java).setAction(ACTION_SESSION).putExtra(EXTRA_HOST, host)
                )
            } catch (e: Throwable) { Log.w(TAG, "session service not allowed: $e") }
        }

        /** The microphone started or stopped: updates the notification and the wake lock. */
        fun micChanged(context: Context, on: Boolean) {
            if (!running) return
            try {
                context.startService(Intent(context, CameraService::class.java).setAction(if (on) ACTION_MIC_ON else ACTION_MIC_OFF))
            } catch (_: Throwable) {}
        }

        /** Re-enters foreground so a newly granted permission gets its service type (only works while the app is visible). */
        fun refresh(context: Context) {
            if (!running) return
            try {
                context.startForegroundService(Intent(context, CameraService::class.java).setAction(ACTION_REFRESH))
            } catch (e: Throwable) { Log.w(TAG, "refresh not allowed: $e") }
        }

        fun endSession(context: Context) {
            if (!running) return
            try { context.startService(Intent(context, CameraService::class.java).setAction(ACTION_END_SESSION)) } catch (_: Throwable) {}
        }
    }

    private var wake: PowerManager.WakeLock? = null
    private var session = false
    private var cameraOn = false
    private var micOn = false
    private var host = ""
    private var foregroundWithCamera = false
    private var foregroundWithMic = false
    private var inForeground = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                CameraRuntime.controller?.stopFromUser()
                AppRuntime.peek()?.disconnect()
                stopSelf()
                return START_NOT_STICKY
            }
            ACTION_SESSION -> { session = true; host = intent.getStringExtra(EXTRA_HOST).orEmpty() }
            ACTION_END_SESSION -> {
                session = false
                micOn = false
                if (!cameraOn) return finish()
            }
            ACTION_CAMERA_OFF -> {
                cameraOn = false
                if (!session) return finish()
            }
            ACTION_MIC_ON -> micOn = true
            ACTION_MIC_OFF -> {
                micOn = false
                if (!session && !cameraOn) return finish()
            }
            ACTION_REFRESH -> Unit
            else -> cameraOn = true // ACTION_CAMERA_ON or a bare start
        }
        sessionUp = session
        updateForeground()
        updateWakeLock()
        return START_NOT_STICKY
    }

    private fun finish(): Int {
        sessionUp = false
        stopSelf()
        return START_NOT_STICKY
    }

    private fun cameraGranted() =
        ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED

    private fun micGranted() =
        ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED

    private fun buildNotification(): Notification {
        val flagsPi = PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), flagsPi)
        val stop = PendingIntent.getService(this, 1, Intent(this, CameraService::class.java).setAction(ACTION_STOP), flagsPi)
        val b = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(this, CHANNEL) else @Suppress("DEPRECATION") Notification.Builder(this)
        val pc = host.ifBlank { "your PC" }
        val title = when {
            cameraOn && micOn -> "Notideck is using the camera and microphone"
            cameraOn -> "Notideck is using the camera"
            micOn -> "Microphone on"
            else -> "Notideck connected to $pc"
        }
        val text = when {
            cameraOn || micOn -> "Streaming to $pc"
            else -> "Ready for the deck and the webcam"
        }
        return b.setContentTitle(title)
            .setContentText(text)
            .setSmallIcon(android.R.drawable.presence_video_online)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setContentIntent(open)
            .addAction(Notification.Action.Builder(null, if (cameraOn && !session) "Stop" else "Disconnect", stop).build())
            .build()
    }

    private fun updateForeground() {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (Build.VERSION.SDK_INT >= 26) {
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL, "Connection and camera", NotificationManager.IMPORTANCE_LOW)
                    .apply { setShowBadge(false) }
            )
        }
        val n = buildNotification()
        val wantCamera = cameraGranted()
        val wantMic = micGranted()
        // Already foreground with everything we can have: just refresh the text. Re-entering foreground
        // with the camera type from the background would be refused.
        if (inForeground && (foregroundWithCamera || !wantCamera) && (foregroundWithMic || !wantMic)) {
            nm.notify(NOTIFICATION_ID, n)
            return
        }
        try {
            if (Build.VERSION.SDK_INT >= 29) {
                var type = if (session || !wantCamera) ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE else 0
                if (wantCamera) type = type or ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA
                // The microphone type exists from API 30; Android 14 throws if it is declared without RECORD_AUDIO.
                if (Build.VERSION.SDK_INT >= 30) {
                    if (wantMic) type = type or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
                }
                startForeground(NOTIFICATION_ID, n, type)
            } else startForeground(NOTIFICATION_ID, n)
            inForeground = true
            foregroundWithCamera = wantCamera || Build.VERSION.SDK_INT < 29
            holdsCamera = foregroundWithCamera
            foregroundWithMic = wantMic || Build.VERSION.SDK_INT < 30
            holdsMic = foregroundWithMic
            running = true
        } catch (e: Throwable) {
            Log.w(TAG, "startForeground refused: $e")
            if (!inForeground) { sessionUp = false; stopSelf() }
        }
    }

    private fun updateWakeLock() {
        val need = cameraOn || micOn
        if (need && wake == null) {
            wake = (getSystemService(Context.POWER_SERVICE) as PowerManager)
                .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "notideck:stream").apply { setReferenceCounted(false); acquire(12 * 3600_000L) }
        } else if (!need) {
            try { wake?.release() } catch (_: Exception) {}
            wake = null
        }
    }

    override fun onDestroy() {
        try { wake?.release() } catch (_: Exception) {}
        wake = null
        running = false
        sessionUp = false
        holdsCamera = false
        holdsMic = false
        super.onDestroy()
        AppRuntime.peek()?.onServiceGone()
    }
}

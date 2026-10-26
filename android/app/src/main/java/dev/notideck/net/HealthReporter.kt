// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import android.os.Build
import android.os.PowerManager
import dev.notideck.proto.v1.DeviceHealth
import dev.notideck.proto.v1.Envelope
import kotlin.math.abs

/** When a new battery/thermal reading is worth sending between the 30 s heartbeats. */
object HealthPolicy {
    const val PERIOD_MS = 30_000L
    const val MIN_GAP_MS = 2_000L
    const val TEMP_STEP_C = 1.0f

    fun significant(prev: DeviceHealth?, cur: DeviceHealth): Boolean =
        prev == null ||
            prev.battery_percent != cur.battery_percent ||
            prev.charging != cur.charging ||
            prev.thermal_throttled != cur.thermal_throttled ||
            abs(prev.battery_temp_c - cur.battery_temp_c) >= TEMP_STEP_C
}

/**
 * Sends [DeviceHealth] to the host: once at start, every 30 s, and on a significant change.
 * [start]/[stop] are called on the main thread by the runtime for the life of a control session.
 */
class HealthReporter(
    context: Context,
    private val send: (Envelope) -> Unit,
    private val isThrottled: () -> Boolean,
    private val now: () -> Long = System::currentTimeMillis
) {
    private val app = context.applicationContext
    private val handler = android.os.Handler(android.os.Looper.getMainLooper())
    private var receiver: BroadcastReceiver? = null
    private var last: DeviceHealth? = null
    private var lastSentAt = 0L

    private val tick = object : Runnable {
        override fun run() {
            emit(force = true)
            handler.postDelayed(this, HealthPolicy.PERIOD_MS)
        }
    }

    fun start() {
        if (receiver != null) return
        last = null
        val r = object : BroadcastReceiver() {
            override fun onReceive(c: Context?, i: Intent?) { emit(force = false, battery = i) }
        }
        receiver = r
        app.registerReceiver(r, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        handler.removeCallbacks(tick)
        handler.post(tick)
    }

    fun stop() {
        handler.removeCallbacks(tick)
        receiver?.let { try { app.unregisterReceiver(it) } catch (_: Exception) {} }
        receiver = null
    }

    private fun emit(force: Boolean, battery: Intent? = null) {
        val i = battery ?: app.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED)) ?: return
        val cur = sample(i)
        val t = now()
        if (!force && (!HealthPolicy.significant(last, cur) || t - lastSentAt < HealthPolicy.MIN_GAP_MS)) return
        last = cur
        lastSentAt = t
        send(Envelope(health = cur))
    }

    private fun sample(i: Intent): DeviceHealth {
        val level = i.getIntExtra(BatteryManager.EXTRA_LEVEL, -1)
        val scale = i.getIntExtra(BatteryManager.EXTRA_SCALE, 100).coerceAtLeast(1)
        val status = i.getIntExtra(BatteryManager.EXTRA_STATUS, -1)
        val plugged = i.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0)
        val temp = i.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, 0) / 10f
        val osHot = Build.VERSION.SDK_INT >= 29 &&
            (app.getSystemService(Context.POWER_SERVICE) as PowerManager).currentThermalStatus >= PowerManager.THERMAL_STATUS_SEVERE
        return DeviceHealth(
            battery_percent = if (level < 0) 0 else (level * 100 / scale).coerceIn(0, 100),
            battery_temp_c = temp,
            charging = plugged != 0 || status == BatteryManager.BATTERY_STATUS_CHARGING || status == BatteryManager.BATTERY_STATUS_FULL,
            thermal_throttled = isThrottled() || osHot
        )
    }
}

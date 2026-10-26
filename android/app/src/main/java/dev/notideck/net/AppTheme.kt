// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import android.content.Context
import dev.notideck.proto.v1.PhoneTheme

/** Phone appearance (host-selected). */
enum class AppTheme {
    Black, Graphite,
    /** Light pastel themes. */
    Cream, Lavender, Mint,
    /** Dark themes with pastel accents. */
    Dusk, Tide;

    companion object {
        val DEFAULT = Black

        /** Maps the wire enum; UNSPECIFIED keeps [current]. */
        fun fromProto(p: PhoneTheme, current: AppTheme): AppTheme = when (p) {
            PhoneTheme.PHONE_THEME_BLACK -> Black
            PhoneTheme.PHONE_THEME_GRAPHITE -> Graphite
            PhoneTheme.PHONE_THEME_CREAM -> Cream
            PhoneTheme.PHONE_THEME_LAVENDER -> Lavender
            PhoneTheme.PHONE_THEME_MINT -> Mint
            PhoneTheme.PHONE_THEME_DUSK -> Dusk
            PhoneTheme.PHONE_THEME_TIDE -> Tide
            else -> current
        }

        fun parse(name: String?): AppTheme = entries.firstOrNull { it.name == name } ?: DEFAULT
    }
}

/** Persistence for the last received theme. */
interface ThemeStore {
    fun load(): AppTheme
    fun save(theme: AppTheme)
}

class MemoryThemeStore(private var value: AppTheme? = null) : ThemeStore {
    override fun load() = value ?: AppTheme.DEFAULT
    override fun save(theme: AppTheme) { value = theme }
}

class PrefsThemeStore(context: Context) : ThemeStore {
    private val prefs = context.applicationContext.getSharedPreferences("notideck", Context.MODE_PRIVATE)
    override fun load() = AppTheme.parse(prefs.getString(KEY, null))
    override fun save(theme: AppTheme) { prefs.edit().putString(KEY, theme.name).apply() }
    private companion object { const val KEY = "phone_theme" }
}

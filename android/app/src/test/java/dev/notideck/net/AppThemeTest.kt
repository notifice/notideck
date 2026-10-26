// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.proto.v1.PhoneTheme
import org.junit.Assert.assertEquals
import org.junit.Test

class AppThemeTest {
    @Test
    fun protoMapping() {
        assertEquals(AppTheme.Black, AppTheme.fromProto(PhoneTheme.PHONE_THEME_BLACK, AppTheme.Graphite))
        assertEquals(AppTheme.Graphite, AppTheme.fromProto(PhoneTheme.PHONE_THEME_GRAPHITE, AppTheme.Black))
    }

    @Test
    fun protoMappingCoversAllThemes() {
        val expected = mapOf(
            PhoneTheme.PHONE_THEME_BLACK to AppTheme.Black,
            PhoneTheme.PHONE_THEME_GRAPHITE to AppTheme.Graphite,
            PhoneTheme.PHONE_THEME_CREAM to AppTheme.Cream,
            PhoneTheme.PHONE_THEME_LAVENDER to AppTheme.Lavender,
            PhoneTheme.PHONE_THEME_MINT to AppTheme.Mint,
            PhoneTheme.PHONE_THEME_DUSK to AppTheme.Dusk,
            PhoneTheme.PHONE_THEME_TIDE to AppTheme.Tide
        )
        assertEquals(AppTheme.entries.toSet(), expected.values.toSet())
        for ((proto, theme) in expected) {
            // Independent of the current value.
            for (current in AppTheme.entries) assertEquals(theme, AppTheme.fromProto(proto, current))
        }
    }

    @Test
    fun parseRoundTripsEveryName() {
        for (t in AppTheme.entries) assertEquals(t, AppTheme.parse(t.name))
        assertEquals(AppTheme.Cream, AppTheme.parse("Cream"))
        assertEquals(AppTheme.Tide, AppTheme.parse("Tide"))
        assertEquals(AppTheme.Black, AppTheme.parse("cream")) // case-sensitive; unknown -> default
    }

    @Test
    fun storePersistsNewThemes() {
        val store = MemoryThemeStore()
        for (t in AppTheme.entries) {
            val client = SessionClient(themeStore = store)
            val proto = PhoneTheme.entries.first { AppTheme.fromProto(it, AppTheme.DEFAULT) == t && it != PhoneTheme.PHONE_THEME_UNSPECIFIED }
            client.applyAppearance(proto)
            assertEquals(t, client.theme.value)
            assertEquals(t, store.load())
        }
    }

    @Test
    fun unspecifiedKeepsCurrent() {
        assertEquals(AppTheme.Graphite, AppTheme.fromProto(PhoneTheme.PHONE_THEME_UNSPECIFIED, AppTheme.Graphite))
        assertEquals(AppTheme.Black, AppTheme.fromProto(PhoneTheme.PHONE_THEME_UNSPECIFIED, AppTheme.Black))
    }

    @Test
    fun defaultIsBlack() {
        assertEquals(AppTheme.Black, AppTheme.DEFAULT)
        assertEquals(AppTheme.Black, MemoryThemeStore().load())
        assertEquals(AppTheme.Black, AppTheme.parse(null))
        assertEquals(AppTheme.Black, AppTheme.parse("garbage"))
        assertEquals(AppTheme.Graphite, AppTheme.parse("Graphite"))
    }

    @Test
    fun sessionClientAppliesAndPersists() {
        val store = MemoryThemeStore()
        val client = SessionClient(themeStore = store)
        assertEquals(AppTheme.Black, client.theme.value)
        client.applyAppearance(PhoneTheme.PHONE_THEME_GRAPHITE)
        assertEquals(AppTheme.Graphite, client.theme.value)
        assertEquals(AppTheme.Graphite, store.load())
        client.applyAppearance(PhoneTheme.PHONE_THEME_UNSPECIFIED)
        assertEquals(AppTheme.Graphite, client.theme.value)
        assertEquals(AppTheme.Graphite, SessionClient(themeStore = store).theme.value)
    }
}

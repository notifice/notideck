// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import android.text.format.DateUtils
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.notideck.net.DiscoveredHost
import dev.notideck.pairing.PairedHost
import dev.notideck.ui.theme.NdColors
import dev.notideck.ui.theme.NdShapes
import dev.notideck.ui.theme.NdType

/** Paired PCs (with Forget), PCs seen on the network, and the way into the QR scanner. */
@Composable
fun PcsScreen(
    hosts: List<PairedHost>,
    discovered: List<DiscoveredHost>,
    onForget: (String) -> Unit,
    onScan: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier
) {
    Column(
        modifier
            .fillMaxSize()
            .background(NdColors.Base)
            .padding(horizontal = 28.dp, vertical = 20.dp)
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("PCs", style = NdType.Display.copy(fontSize = 30.sp, lineHeight = 30.sp), color = NdColors.Text)
            Spacer(Modifier.weight(1f))
            NdButton("Scan QR", onScan, primary = true)
            Spacer(Modifier.width(8.dp))
            NdButton("Back", onBack, primary = false)
        }
        Spacer(Modifier.height(12.dp))
        Box(Modifier.fillMaxWidth().height(1.dp).background(NdColors.Hairline))
        LazyColumn(Modifier.fillMaxSize(), contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 14.dp)) {
            item { SectionLabel("Paired") }
            if (hosts.isEmpty()) {
                item {
                    Text(
                        "None yet. Plug in USB and allow the phone on the PC, or scan the QR code from the PC's Devices tab.",
                        style = NdType.Micro, color = NdColors.TextDim, modifier = Modifier.padding(bottom = 10.dp)
                    )
                }
            }
            items(hosts, key = { "p" + it.hostId }) { h -> PairedRow(h, onForget) }
            item { Spacer(Modifier.height(10.dp)); SectionLabel("On this network") }
            if (discovered.isEmpty()) {
                item { Text("Nothing found.", style = NdType.Micro, color = NdColors.TextFaint) }
            }
            items(discovered, key = { "d" + it.service.serviceName }) { d -> DiscoveredRow(d) }
        }
    }
}


@Composable
private fun SectionLabel(text: String) {
    Text(text, style = NdType.Label, color = NdColors.TextDim, modifier = Modifier.padding(bottom = 8.dp))
}

@Composable
private fun PairedRow(h: PairedHost, onForget: (String) -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .padding(bottom = 8.dp)
            .background(NdColors.Panel, NdShapes.Panel)
            .border(1.dp, NdColors.Hairline, NdShapes.Panel)
            .padding(horizontal = 14.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Column(Modifier.weight(1f)) {
            Text(h.name.ifBlank { h.hostId }, style = NdType.KeyTitle, color = NdColors.Text, maxLines = 1)
            Spacer(Modifier.height(3.dp))
            Text(
                "Last seen " + lastSeen(h.lastConnected) + "  \u00b7  fp " + h.fp.take(8),
                style = NdType.Micro, color = NdColors.TextDim, maxLines = 1
            )
        }
        NdButton("Forget", { onForget(h.hostId) }, primary = false)
    }
}

@Composable
private fun DiscoveredRow(d: DiscoveredHost) {
    Row(
        Modifier
            .fillMaxWidth()
            .padding(bottom = 8.dp)
            .background(NdColors.Panel, NdShapes.Panel)
            .border(1.dp, NdColors.Hairline, NdShapes.Panel)
            .padding(horizontal = 14.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Column(Modifier.weight(1f)) {
            Text(d.service.name, style = NdType.KeyTitle, color = NdColors.Text, maxLines = 1)
            Spacer(Modifier.height(3.dp))
            Text(
                d.service.addrs.joinToString() + ":" + d.service.port,
                style = NdType.Micro, color = NdColors.TextDim, maxLines = 1
            )
        }
        Text(
            if (d.isPaired) "Paired" else "Scan QR to pair",
            style = NdType.Micro,
            color = if (d.isPaired) NdColors.AccentInk else NdColors.TextDim
        )
    }
}

private fun lastSeen(ms: Long): String =
    if (ms <= 0) "never" else DateUtils.getRelativeTimeSpanString(ms, System.currentTimeMillis(), DateUtils.MINUTE_IN_MILLIS).toString()

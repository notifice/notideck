// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import android.content.Context
import androidx.activity.compose.BackHandler
import androidx.annotation.OptIn
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.common.PlaybackException
import androidx.media3.common.PlaybackParameters
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.DefaultHttpDataSource
import androidx.media3.exoplayer.DefaultLoadControl
import androidx.media3.exoplayer.DefaultRenderersFactory
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.hls.HlsMediaSource
import androidx.media3.exoplayer.trackselection.DefaultTrackSelector
import androidx.media3.ui.AspectRatioFrameLayout
import androidx.media3.ui.PlayerView
import dev.notideck.net.PlayerRequest
import dev.notideck.ui.theme.NdColors
import dev.notideck.ui.theme.NdShapes
import dev.notideck.ui.theme.NdType

private const val OVERLAY_HIDE_MS = 3000L

/** Red used by the LIVE tally pill on tiles and in the player. */
internal val LiveRed = Color(0xFFE5484D)

/** Short, user-facing text for a playback failure. */
internal fun playbackErrorMessage(e: PlaybackException): String = when (e.errorCode) {
    PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED,
    PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_TIMEOUT -> "No connection to the stream"
    PlaybackException.ERROR_CODE_IO_BAD_HTTP_STATUS,
    PlaybackException.ERROR_CODE_IO_FILE_NOT_FOUND -> "The stream is not available (it may have ended)"
    PlaybackException.ERROR_CODE_BEHIND_LIVE_WINDOW -> "Fell behind the live stream"
    PlaybackException.ERROR_CODE_PARSING_CONTAINER_MALFORMED,
    PlaybackException.ERROR_CODE_PARSING_MANIFEST_MALFORMED -> "The stream could not be read"
    else -> "Playback failed"
}

@OptIn(UnstableApi::class)
internal fun buildPlayer(context: Context, request: PlayerRequest): ExoPlayer {
    val selector = DefaultTrackSelector(context).apply {
        if (request.maxHeight > 0) {
            setParameters(buildUponParameters().setMaxVideoSize(Int.MAX_VALUE, request.maxHeight))
        }
    }
    val http = DefaultHttpDataSource.Factory()
        .setUserAgent("Notideck-Android")
        .setConnectTimeoutMs(10_000)
        .setReadTimeoutMs(10_000)
        .setAllowCrossProtocolRedirects(true)
    // Live tuning: ExoPlayer's defaults wait for 5 s of buffer after any stall, but a live stream held
    // a few seconds behind its edge often never has 5 s ahead, so it sat in "loading" until it fell
    // behind and the sync loop seeked again. Start on 1 s, resume on 1.5 s, and keep a back buffer.
    val load = DefaultLoadControl.Builder()
        .setBufferDurationsMs(
            /* minBufferMs = */ 15_000,
            /* maxBufferMs = */ 30_000,
            /* bufferForPlaybackMs = */ 1_000,
            /* bufferForPlaybackAfterRebufferMs = */ 1_500
        )
        .setBackBuffer(10_000, /* retainBackBufferFromKeyframe = */ true)
        .setPrioritizeTimeOverSizeThresholds(true)
        .build()
    // Older phones (Xperia XZ, Android 8): fall back to another decoder when the first one fails to init.
    val renderers = DefaultRenderersFactory(context).setEnableDecoderFallback(true)
    return ExoPlayer.Builder(context, renderers)
        .setLoadControl(load)
        .setTrackSelector(selector)
        .setMediaSourceFactory(HlsMediaSource.Factory(http))
        .setAudioAttributes(
            AudioAttributes.Builder()
                .setUsage(C.USAGE_MEDIA)
                .setContentType(C.AUDIO_CONTENT_TYPE_MOVIE)
                .build(),
            /* handleAudioFocus = */ !request.muted
        )
        .setHandleAudioBecomingNoisy(true)
        .build()
        .also { if (request.muted) it.volume = 0f }
}

@OptIn(UnstableApi::class)
internal fun mediaItemFor(request: PlayerRequest): MediaItem =
    MediaItem.Builder()
        .setUri(request.url)
        .setMimeType(androidx.media3.common.MimeTypes.APPLICATION_M3U8)
        .setMediaMetadata(MediaMetadata.Builder().setTitle(request.title).setSubtitle(request.subtitle).build())
        .apply {
            if (request.syncDelayMs > 0) {
                // Built-in live catch-up off: our SyncController owns the rate. Start near the target.
                setLiveConfiguration(
                    MediaItem.LiveConfiguration.Builder()
                        .setTargetOffsetMs(request.syncDelayMs.toLong())
                        .setMinPlaybackSpeed(1f)
                        .setMaxPlaybackSpeed(1f)
                        .build()
                )
            }
        }
        .build()

private const val SYNC_TICK_MS = 500L

/** Wall-clock time of the frame being shown, or null when the stream has no program date time. */
internal fun shownWallMs(player: ExoPlayer): Long? {
    val tl = player.currentTimeline
    if (tl.isEmpty) return null
    val w = tl.getWindow(player.currentMediaItemIndex, androidx.media3.common.Timeline.Window())
    if (w.windowStartTimeMs == C.TIME_UNSET) return null
    return w.windowStartTimeMs + player.currentPosition
}

/**
 * Full-screen HLS player drawn over the deck. A new [request] (different id) replaces the media item;
 * the player is released when this leaves composition or the activity stops. Back / the close button
 * call [onClose].
 */
@OptIn(UnstableApi::class)
@Composable
fun StreamPlayer(
    request: PlayerRequest,
    onClose: () -> Unit,
    modifier: Modifier = Modifier,
    hostClockOffsetMs: () -> Long = { 0L }
) {
    val context = LocalContext.current
    // One ExoPlayer per player session; rebuilt only when the quality cap changes.
    val player = remember(request.maxHeight) { buildPlayer(context, request) }
    var buffering by remember { mutableStateOf(true) }
    var error by remember { mutableStateOf<String?>(null) }
    var overlayVisible by remember { mutableStateOf(true) }
    var overlayTick by remember { mutableIntStateOf(0) }

    DisposableEffect(player) {
        val l = object : Player.Listener {
            override fun onPlaybackStateChanged(state: Int) {
                buffering = state == Player.STATE_BUFFERING || state == Player.STATE_IDLE && error == null
                if (state == Player.STATE_READY) error = null
            }
            override fun onPlayerError(e: PlaybackException) {
                if (e.errorCode == PlaybackException.ERROR_CODE_BEHIND_LIVE_WINDOW) {
                    // Normal for live after a long stall: jump back to the live position and go on.
                    player.seekToDefaultPosition()
                    player.prepare()
                    return
                }
                error = playbackErrorMessage(e)
                buffering = false
                overlayVisible = true
            }
        }
        player.addListener(l)
        onDispose {
            player.removeListener(l)
            player.release()
        }
    }

    // A new request (even the same URL) replaces whatever is playing.
    LaunchedEffect(player, request.id) {
        error = null
        buffering = true
        player.setMediaItem(mediaItemFor(request))
        player.prepare()
        player.playWhenReady = true
        overlayVisible = true
        overlayTick++
    }

    // Synced playback (muted PC-audio mode): every 500 ms nudge rate / seek toward now - delay.
    LaunchedEffect(player, request.id) {
        if (request.syncDelayMs <= 0) return@LaunchedEffect
        val ctl = dev.notideck.net.SyncController()
        while (true) {
            kotlinx.coroutines.delay(SYNC_TICK_MS)
            if (player.playbackState != Player.STATE_READY || !player.isPlaying) continue
            val shown = shownWallMs(player) ?: continue
            val target = System.currentTimeMillis() + hostClockOffsetMs() - request.syncDelayMs
            val headroom = (player.bufferedPosition - player.currentPosition).coerceAtLeast(0L)
            when (val a = ctl.update(shown - target, headroom)) {
                is dev.notideck.net.SyncController.Action.None -> {}
                is dev.notideck.net.SyncController.Action.Rate ->
                    player.playbackParameters = PlaybackParameters(a.rate)
                is dev.notideck.net.SyncController.Action.Seek -> {
                    // Forward and inside the buffer only (the controller caps it), so no network refetch.
                    player.playbackParameters = PlaybackParameters(1f)
                    player.seekTo(player.currentPosition + a.deltaMs)
                }
                is dev.notideck.net.SyncController.Action.Hold -> {
                    // Ahead of the PC audio: freeze the picture and let the clock catch up. A backward seek
                    // would flush the decoder and show the spinner; a pause keeps buffering.
                    player.playbackParameters = PlaybackParameters(1f)
                    player.playWhenReady = false
                    kotlinx.coroutines.delay(a.ms) // cancelled on close: the player is released anyway
                    player.playWhenReady = true
                }
            }
        }
    }

    // Closing is fine when the app leaves the foreground: no resume.
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    DisposableEffect(lifecycle) {
        val obs = LifecycleEventObserver { _, e -> if (e == Lifecycle.Event.ON_STOP) onClose() }
        lifecycle.addObserver(obs)
        onDispose { lifecycle.removeObserver(obs) }
    }

    BackHandler(onBack = onClose)

    // Auto-hide the overlay ~3 s after the last tap; keep it while there is an error.
    LaunchedEffect(overlayVisible, overlayTick, error) {
        if (overlayVisible && error == null) {
            kotlinx.coroutines.delay(OVERLAY_HIDE_MS)
            overlayVisible = false
        }
    }

    Box(
        modifier
            .fillMaxSize()
            .background(Color.Black)
            .clickable(interactionSource = remember { MutableInteractionSource() }, indication = null) {
                overlayVisible = !overlayVisible
                overlayTick++
            }
    ) {
        AndroidView(
            factory = { ctx ->
                PlayerView(ctx).apply {
                    useController = false
                    resizeMode = AspectRatioFrameLayout.RESIZE_MODE_FIT
                    setKeepContentOnPlayerReset(true)
                    setShutterBackgroundColor(android.graphics.Color.BLACK)
                    keepScreenOn = true
                    this.player = player
                    // Taps are handled by the Compose layer above.
                    isClickable = false
                    isFocusable = false
                }
            },
            update = { it.player = player },
            onRelease = { it.player = null },
            modifier = Modifier.fillMaxSize()
        )

        if (buffering && error == null) {
            CircularProgressIndicator(
                color = NdColors.Accent,
                strokeWidth = 3.dp,
                modifier = Modifier.align(Alignment.Center).size(36.dp)
            )
        }

        error?.let { msg ->
            Column(
                Modifier
                    .align(Alignment.Center)
                    .padding(24.dp)
                    .background(NdColors.Panel.copy(alpha = 0.92f), NdShapes.Panel)
                    .padding(18.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(10.dp)
            ) {
                Text(msg, style = NdType.Label, color = NdColors.Text)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    NdButton("Retry", {
                        error = null
                        buffering = true
                        player.setMediaItem(mediaItemFor(request))
                        player.prepare()
                        player.playWhenReady = true
                    }, primary = true)
                    NdButton("Close", onClose, primary = false)
                }
            }
        }

        AnimatedVisibility(
            visible = overlayVisible,
            enter = fadeIn(), exit = fadeOut(),
            modifier = Modifier.align(Alignment.TopStart).fillMaxWidth()
        ) {
            Row(
                Modifier
                    .fillMaxWidth()
                    .background(Brush.verticalGradient(listOf(Color.Black.copy(alpha = 0.75f), Color.Transparent)))
                    .padding(start = 16.dp, end = 8.dp, top = 10.dp, bottom = 24.dp),
                verticalAlignment = Alignment.Top
            ) {
                Column(Modifier.weight(1f)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            request.title.ifEmpty { "Stream" },
                            style = NdType.KeyTitle, color = Color.White,
                            maxLines = 1, overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.weight(1f, fill = false)
                        )
                        Spacer(Modifier.width(8.dp))
                        LivePill()
                    }
                    if (request.muted) {
                        Text(
                            "Sound on PC",
                            style = NdType.Micro, color = NdColors.Accent.copy(alpha = 0.85f),
                            maxLines = 1
                        )
                    }
                    if (request.subtitle.isNotEmpty()) {
                        Text(
                            request.subtitle,
                            style = NdType.Micro, color = Color.White.copy(alpha = 0.78f),
                            maxLines = 1, overflow = TextOverflow.Ellipsis
                        )
                    }
                }
                Box(
                    Modifier
                        .size(44.dp)
                        .clickable(onClick = onClose),
                    contentAlignment = Alignment.Center
                ) {
                    Canvas(Modifier.size(18.dp)) {
                        val sw = Stroke(2.dp.toPx(), cap = StrokeCap.Round)
                        drawLine(Color.White, Offset(0f, 0f), Offset(size.width, size.height), sw.width, sw.cap)
                        drawLine(Color.White, Offset(size.width, 0f), Offset(0f, size.height), sw.width, sw.cap)
                    }
                }
            }
        }
    }
}

/** Small red "LIVE" tally pill. */
@Composable
internal fun LivePill(modifier: Modifier = Modifier, compact: Boolean = false) {
    Row(
        modifier
            .background(LiveRed, NdShapes.Pill)
            .padding(horizontal = if (compact) 6.dp else 8.dp, vertical = if (compact) 1.dp else 2.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Box(Modifier.size(if (compact) 4.dp else 5.dp).background(Color.White, CircleShape))
        Spacer(Modifier.width(if (compact) 3.dp else 4.dp))
        Text(
            "LIVE",
            style = NdType.Micro.copy(
                fontSize = if (compact) 9.sp else 10.sp,
                lineHeight = if (compact) 11.sp else 12.sp,
                letterSpacing = 0.6.sp
            ),
            color = Color.White,
            maxLines = 1
        )
    }
}

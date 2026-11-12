package app.hghub.mobile.ui

import android.app.Activity
import android.content.Context
import android.os.Build
import android.content.ContextWrapper
import android.content.pm.ActivityInfo
import android.graphics.drawable.ClipDrawable
import android.graphics.drawable.GradientDrawable
import android.graphics.drawable.InsetDrawable
import android.graphics.drawable.LayerDrawable
import android.os.SystemClock
import android.view.Choreographer
import android.view.Gravity
import android.view.ViewGroup
import android.widget.SeekBar
import androidx.annotation.OptIn
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.pager.VerticalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.outlined.FavoriteBorder
import androidx.compose.material.icons.filled.Fullscreen
import androidx.compose.material.icons.filled.FullscreenExit
import androidx.compose.material.icons.filled.PictureInPictureAlt
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.outlined.List
import androidx.compose.material.icons.outlined.StarOutline
import androidx.compose.material.icons.outlined.VolumeOff
import androidx.compose.material.icons.outlined.VolumeUp
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.VideoSize
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.okhttp.OkHttpDataSource
import app.hghub.mobile.data.safeHttp
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.DefaultMediaSourceFactory
import androidx.media3.ui.AspectRatioFrameLayout
import androidx.media3.ui.PlayerView
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.core.app.PictureInPictureModeChangedInfo
import androidx.core.util.Consumer
import app.hghub.mobile.pipParams
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import app.hghub.mobile.Library
import app.hghub.mobile.LocalApp
import app.hghub.mobile.data.Comment
import app.hghub.mobile.data.Detail
import app.hghub.mobile.data.PlayInfo
import app.hghub.mobile.data.fmtClock
import app.hghub.mobile.data.fmtCount
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import androidx.compose.runtime.snapshotFlow

private val speeds = floatArrayOf(1f, 1.25f, 1.5f, 2f)

@OptIn(UnstableApi::class)
@kotlin.OptIn(ExperimentalMaterial3Api::class)
@Composable
fun VideoFeed(
    ids: List<Int>,
    start: Int,
    startEp: Int,
    startTimeSec: Double,
    onBack: (() -> Unit)?,
    onNearEnd: (() -> Unit)?,
    episodePaging: Boolean = false,
    onEnterSeries: ((Int, Int, Double) -> Unit)? = null,
    progressOf: (Int) -> Pair<Int, Double>? = { null },
    mini: Boolean = false,
    onMini: ((Int, Int, Double) -> Unit)? = null,
) {
    val app = LocalApp.current
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val base = app.store.base
    if (ids.isEmpty()) {
        EmptyHint("暂无视频", "下拉列表为空", "返回", onBack)
        return
    }
    var episodeTotal by remember {
        mutableIntStateOf(maxOf(startEp, Library.get(ids.firstOrNull() ?: 0)?.episodeCount ?: 1, 1))
    }
    val pager = rememberPagerState(
        initialPage = if (episodePaging) (startEp - 1).coerceAtLeast(0) else start.coerceIn(0, ids.lastIndex),
        pageCount = { if (episodePaging) episodeTotal.coerceAtLeast(1) else ids.size.coerceAtLeast(1) },
    )
    val dataSource = remember {
        OkHttpDataSource.Factory(safeHttp)
            .setUserAgent(HgApiUa)
    }
    val player = remember {
        ExoPlayer.Builder(context)
            .setMediaSourceFactory(DefaultMediaSourceFactory(dataSource))
            .build()
            .apply {
                playWhenReady = true
                volume = if (app.store.muted) 0f else 1f
            }
    }
    DisposableEffect(player) { onDispose { player.release() } }

    var pos by remember { mutableLongStateOf(0L) }
    var dur by remember { mutableLongStateOf(0L) }
    var buffering by remember { mutableStateOf(true) }
    var switching by remember { mutableStateOf(true) }
    var lineAttempt by remember { mutableIntStateOf(0) }
    var lineFor by remember { mutableStateOf("") }
    var failed by remember { mutableStateOf("") }
    var paused by remember { mutableStateOf(false) }
    var speedIndex by remember { mutableIntStateOf(0) }
    var sheet by remember { mutableStateOf<SheetKind?>(null) }
    var detail by remember { mutableStateOf<Detail?>(null) }
    var ep by remember(ids.getOrNull(pager.currentPage)) {
        val saved = if (episodePaging) null else ids.getOrNull(pager.currentPage)?.let(progressOf)
        mutableIntStateOf(
            when {
                pager.currentPage == start -> startEp.coerceAtLeast(1)
                saved != null -> saved.first.coerceAtLeast(1)
                else -> 1
            },
        )
    }
    var picked by remember(ids.getOrNull(if (episodePaging) 0 else pager.currentPage)) { mutableStateOf(false) }
    var commentExtra by remember(if (episodePaging) ids.firstOrNull() else ids.getOrNull(pager.currentPage)) { mutableIntStateOf(0) }
    val comments = remember { androidx.compose.runtime.mutableStateListOf<Comment>() }
    var commentText by remember { mutableStateOf("") }
    var resume by remember { mutableStateOf(if (startTimeSec > 0.3) startTimeSec else 0.0) }
    var ticket by remember { mutableIntStateOf(0) }
    var fullscreen by remember { mutableStateOf(false) }
    var chrome by remember { mutableStateOf(true) }
    var chromeTick by remember { mutableIntStateOf(0) }
    var hevcUrl by remember { mutableStateOf("") }
    var qualityKnown by remember { mutableStateOf(false) }
    var playGen by remember { mutableIntStateOf(0) }

    val onEpisodeEnded = remember { EndBridge() }

    DisposableEffect(player) {
        val listener = object : Player.Listener {
            override fun onPlaybackStateChanged(state: Int) {
                buffering = state == Player.STATE_BUFFERING || state == Player.STATE_IDLE
                if (state == Player.STATE_READY && resume > 0.3) {
                    player.seekTo((resume * 1000).toLong())
                    resume = 0.0
                }
                if (state == Player.STATE_ENDED) onEpisodeEnded.go()
            }
            override fun onPlayerError(error: PlaybackException) {
                if (app.store.preferHevc && hevcUrl.isNotBlank()) {
                    val stay = player.currentPosition
                    if (stay > 300) resume = stay / 1000.0
                    app.store.applyPreferHevc(false)
                    app.store.show("高清暂时播不了，已切回标准")
                    failed = ""
                    return
                }
                val stay = player.currentPosition
                if (stay > 300) resume = stay / 1000.0
                if (lineAttempt < 1) {
                    lineAttempt += 1
                    failed = ""
                    app.store.show("正在重新获取播放地址")
                } else {
                    failed = "这一集暂时播不了，可稍后重试"
                    switching = false
                    buffering = false
                }
            }
            override fun onIsPlayingChanged(isPlaying: Boolean) {
                paused = !isPlaying && player.playbackState == Player.STATE_READY
            }
        }
        player.addListener(listener)
        onDispose { player.removeListener(listener) }
    }

    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val activity = context.findActivity() as? ComponentActivity
    var inPip by remember { mutableStateOf(activity?.isInPictureInPictureMode == true) }
    var scrubbing by remember { mutableStateOf(false) }
    var wide by remember { mutableStateOf(false) }
    DisposableEffect(player) {
        fun apply(size: VideoSize) {
            wide = size.height > 0 && size.width * size.pixelWidthHeightRatio > size.height
        }
        val listener = object : Player.Listener {
            override fun onVideoSizeChanged(videoSize: VideoSize) = apply(videoSize)
            override fun onMediaItemTransition(mediaItem: MediaItem?, reason: Int) {
                wide = false
            }
        }
        player.addListener(listener)
        apply(player.videoSize)
        onDispose { player.removeListener(listener) }
    }
    DisposableEffect(activity) {
        if (activity == null) return@DisposableEffect onDispose {}
        val listener = Consumer<PictureInPictureModeChangedInfo> { info ->
            inPip = info.isInPictureInPictureMode
            app.store.inPip = info.isInPictureInPictureMode
        }
        activity.addOnPictureInPictureModeChangedListener(listener)
        onDispose { activity.removeOnPictureInPictureModeChangedListener(listener) }
    }
    DisposableEffect(lifecycle, player) {
        val obs = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_STOP && activity?.isInPictureInPictureMode != true) player.pause()
        }
        lifecycle.addObserver(obs)
        onDispose {
            lifecycle.removeObserver(obs)
            app.store.canDesktopPip = false
        }
    }
    LaunchedEffect(player, app.store.pipOnDesktop, inPip) {
        while (true) {
            val size = player.videoSize
            if (size.width > 0 && size.height > 0) {
                app.store.pipWidth = size.width
                app.store.pipHeight = size.height
            }
            val active = player.mediaItemCount > 0 && player.playbackState != Player.STATE_IDLE
            app.store.canDesktopPip = active
            if (Build.VERSION.SDK_INT >= 26 && activity != null) {
                activity.setPictureInPictureParams(
                    pipParams(app.store.pipOnDesktop, active, app.store.pipWidth, app.store.pipHeight),
                )
            }
            delay(500)
        }
    }

    LaunchedEffect(player) {
        while (true) {
            pos = player.currentPosition.coerceAtLeast(0L)
            val d = player.duration
            dur = if (d > 0) d else 0L
            val id = if (episodePaging) ids.firstOrNull() ?: 0 else ids.getOrNull(pager.currentPage) ?: 0
            val episode = if (episodePaging) pager.currentPage + 1 else ep
            if (id > 0 && dur > 0 && pos > 800) {
                app.store.saveWatch(id, episode, pos / 1000.0, dur / 1000.0, keepFurther = !episodePaging && !picked)
            }
            delay(800)
        }
    }

    LaunchedEffect(pager.currentPage, ids.size, episodePaging) {
        if (!episodePaging && pager.currentPage >= ids.size - 2) onNearEnd?.invoke()
    }

    val currentId = if (episodePaging) ids.first() else ids[pager.currentPage.coerceIn(0, ids.lastIndex)]
    val playingEp = if (episodePaging) pager.currentPage + 1 else ep
    SideEffect {
        onEpisodeEnded.go = go@{
            val now = SystemClock.uptimeMillis()
            if (now - onEpisodeEnded.at < 700) return@go
            onEpisodeEnded.at = now
            val total = if (episodePaging) {
                episodeTotal.coerceAtLeast(1)
            } else {
                (detail?.episodes?.size?.takeIf { it > 0 } ?: detail?.episodeCount ?: Library.get(currentId)?.episodeCount ?: playingEp).coerceAtLeast(1)
            }
            if (playingEp < total) {
                resume = 0.0
                if (episodePaging) {
                    scope.launch { pager.scrollToPage(playingEp.coerceIn(0, (episodeTotal - 1).coerceAtLeast(0))) }
                } else {
                    picked = true
                    ep = playingEp + 1
                }
            } else {
                player.seekTo(0)
                player.play()
            }
        }
    }

    LaunchedEffect(episodePaging, startEp) {
        if (!episodePaging) return@LaunchedEffect
        val page = (startEp - 1).coerceIn(0, (episodeTotal - 1).coerceAtLeast(0))
        if (pager.currentPage != page) pager.scrollToPage(page)
    }

    LaunchedEffect(currentId, playingEp, episodePaging) {
        if (currentId <= 0 || !episodePaging) return@LaunchedEffect
        delay(400)
        if (pager.currentPage + 1 != playingEp) return@LaunchedEffect
        val prev = app.store.history.firstOrNull { it.id == currentId }
        val t = if (prev?.ep == playingEp) prev.t else 0.0
        app.store.saveWatch(currentId, playingEp, t, prev?.dur ?: 0.0)
    }

    LaunchedEffect(pager.currentPage, episodePaging) {
        if (episodePaging) return@LaunchedEffect
        if (pager.currentPage == start) {
            resume = if (startTimeSec > 0.3) startTimeSec else 0.0
            return@LaunchedEffect
        }
        val saved = ids.getOrNull(pager.currentPage)?.let(progressOf)
        resume = if (saved != null && saved.second > 1) saved.second else 0.0
    }
    BackHandler(enabled = fullscreen) { fullscreen = false }

    LaunchedEffect(fullscreen, chrome, chromeTick) {
        if (!fullscreen || !chrome) return@LaunchedEffect
        delay(3500)
        chrome = false
    }

    DisposableEffect(fullscreen, inPip) {
        val host = context.findActivity()
        val window = host?.window
        if (!inPip) {
            host?.requestedOrientation = if (fullscreen) {
                ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
            } else {
                ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
            }
        }
        val controller = window?.let { WindowCompat.getInsetsController(it, it.decorView) }
        if (fullscreen) {
            controller?.hide(WindowInsetsCompat.Type.systemBars())
            controller?.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        } else {
            controller?.show(WindowInsetsCompat.Type.systemBars())
        }
        app.store.immersive = fullscreen
        onDispose {
            app.store.immersive = false
            if (host?.isInPictureInPictureMode != true) {
                host?.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
            }
            if (window != null) {
                WindowCompat.getInsetsController(window, window.decorView).show(WindowInsetsCompat.Type.systemBars())
            }
        }
    }

    LaunchedEffect(currentId, playingEp, base, ticket, app.store.preferHevc, lineAttempt) {
        val id = currentId
        val episode = playingEp
        val marker = "$id:$episode"
        if (lineFor != marker) {
            lineFor = marker
            if (lineAttempt != 0) {
                lineAttempt = 0
                return@LaunchedEffect
            }
        }
        switching = true
        failed = ""
        delay(180)
        snapshotFlow { pager.isScrollInProgress }.first { !it }
        if (episodePaging) {
            if (pager.currentPage + 1 != episode) return@LaunchedEffect
        } else if (ids.getOrNull(pager.currentPage) != id) {
            return@LaunchedEffect
        }
        buffering = true
        val loadedDetail = try {
            app.api.video(id)
        } catch (e: CancellationException) {
            throw e
        } catch (_: Exception) {
            null
        }
        if (!stillCurrent(episodePaging, pager.currentPage, ids, id, episode)) return@LaunchedEffect
        if (loadedDetail != null) detail = loadedDetail
        if (episodePaging && loadedDetail != null) {
            val total = loadedDetail.episodes.size.takeIf { it > 0 } ?: loadedDetail.episodeCount
            if (total > 0 && total != episodeTotal) episodeTotal = total
            if (total > 0 && episode > total) {
                pager.scrollToPage(total - 1)
                return@LaunchedEffect
            }
        }
        if (loadedDetail?.liked == true && !app.store.isLiked(id)) app.store.toggleLike(id)
        if (loadedDetail?.collected == true && !app.store.isFav(id)) app.store.toggleFav(id)
        var playInfo: PlayInfo? = null
        var lastError = "没有可播放的地址"
        for (attempt in 0..1) {
            if (!stillCurrent(episodePaging, pager.currentPage, ids, id, episode)) return@LaunchedEffect
            try {
                val info = app.api.playback(id, episode)
                if (info.videoUrl.isNotBlank() || info.hevcUrl.isNotBlank()) {
                    playInfo = info
                    break
                }
                lastError = "没有可播放的地址"
                break
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                lastError = e.message ?: lastError
                if (attempt == 1 || !playRetryable(lastError)) break
                delay(700)
            }
        }
        if (playInfo == null && stillCurrent(episodePaging, pager.currentPage, ids, id, episode)) {
            val legacy = try {
                app.api.play(id, episode)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                lastError = e.message ?: lastError
                null
            }
            if (legacy != null && (legacy.videoUrl.isNotBlank() || legacy.hevcUrl.isNotBlank())) playInfo = legacy
        }
        if (playInfo == null && !loadedDetail?.videoUrl.isNullOrBlank()) {
            playInfo = PlayInfo(
                id, episode, loadedDetail?.title.orEmpty(), 0, loadedDetail?.videoUrl.orEmpty(), "",
            )
        }
        val play = playInfo
        if (!stillCurrent(episodePaging, pager.currentPage, ids, id, episode)) return@LaunchedEffect
        if (play == null) {
            failed = lastError
            switching = false
            buffering = false
            return@LaunchedEffect
        }
        hevcUrl = play.hevcUrl
        qualityKnown = true
        val standard = play.videoUrl.ifBlank { loadedDetail?.videoUrl.orEmpty() }
        val url = if (app.store.preferHevc && play.hevcUrl.isNotBlank()) play.hevcUrl else standard
        if (url.isBlank()) {
            failed = "没有可播放的地址"
            switching = false
            buffering = false
            return@LaunchedEffect
        }
        val origin = base.trim().trimEnd('/')
        val headers = buildMap {
            put("Referer", "$origin/")
            put("Origin", origin)
            if (play.cdnHeader == "x-cdn-auth" && play.cdnValue.isNotBlank()) put(play.cdnHeader, play.cdnValue)
        }
        dataSource.setDefaultRequestProperties(headers)
        playGen += 1
        player.setMediaItem(MediaItem.fromUri(url))
        player.prepare()
        player.playWhenReady = true
        player.setPlaybackSpeed(speeds[speedIndex])
        player.volume = if (app.store.muted) 0f else 1f
        switching = false
    }

    LaunchedEffect(playGen) {
        if (playGen == 0) return@LaunchedEffect
        delay(9000)
        if (player.playbackState != Player.STATE_BUFFERING || player.currentPosition > 800) return@LaunchedEffect
        if (lineAttempt >= 1) return@LaunchedEffect
        val stay = player.currentPosition
        if (stay > 300) resume = stay / 1000.0
        lineAttempt += 1
        app.store.show("正在重新获取播放地址")
    }

    @Composable
    fun TopControls() {
        Row(
            Modifier.fillMaxWidth().then(if (fullscreen) Modifier else Modifier.statusBarsPadding()).padding(horizontal = 8.dp, vertical = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (fullscreen || onBack != null) {
                Icon(
                    Icons.Filled.ArrowBack,
                    "返回",
                    tint = Color.White,
                    modifier = Modifier.size(40.dp).clickable {
                        if (fullscreen) fullscreen = false else onBack?.invoke()
                    }.padding(8.dp),
                )
                if (episodePaging || fullscreen) {
                    Text("第 $playingEp 集", color = Color.White, fontFamily = Serif, fontSize = 18.sp)
                }
            } else {
                Text("HGhub", color = Color.White, fontFamily = Serif, fontSize = 22.sp, modifier = Modifier.padding(start = 8.dp))
            }
            Spacer(Modifier.weight(1f))
            QualityMenu(
                preferHevc = app.store.preferHevc,
                hevcReady = hevcUrl.isNotBlank(),
                known = qualityKnown,
                onPick = { wantHevc ->
                    chromeTick += 1
                    if (wantHevc && qualityKnown && hevcUrl.isBlank()) {
                        app.store.show("这一集没有高清")
                        return@QualityMenu
                    }
                    if (wantHevc == app.store.preferHevc) return@QualityMenu
                    val stay = player.currentPosition
                    if (stay > 300) resume = stay / 1000.0
                    app.store.applyPreferHevc(wantHevc)
                },
            )
            Spacer(Modifier.width(8.dp))
            Icon(
                Icons.Filled.PictureInPictureAlt,
                "小窗",
                tint = if (app.store.pipOnDesktop) Color(0xFFE08A6E) else Color.White,
                modifier = Modifier.size(40.dp).clickable {
                    app.store.applyPipOnDesktop(!app.store.pipOnDesktop)
                }.padding(8.dp),
            )
            Icon(
                if (fullscreen) Icons.Filled.FullscreenExit else Icons.Filled.Fullscreen,
                if (fullscreen) "退出全屏" else "全屏",
                tint = Color.White,
                modifier = Modifier.size(40.dp).clickable {
                    fullscreen = !fullscreen
                    chrome = true
                    chromeTick += 1
                }.padding(8.dp),
            )
            Text(
                "${speeds[speedIndex]}x",
                color = Color.White,
                fontSize = 13.sp,
                modifier = Modifier
                    .clip(RoundedCornerShape(999.dp))
                    .background(Color(0x66000000))
                    .clickable {
                        speedIndex = (speedIndex + 1) % speeds.size
                        player.setPlaybackSpeed(speeds[speedIndex])
                        chromeTick += 1
                    }
                    .padding(horizontal = 12.dp, vertical = 6.dp),
            )
            Spacer(Modifier.width(8.dp))
            Icon(
                if (app.store.muted) Icons.Outlined.VolumeOff else Icons.Outlined.VolumeUp,
                "静音",
                tint = Color.White,
                modifier = Modifier.size(40.dp).clickable {
                    val next = !app.store.muted
                    app.store.applyMuted(next)
                    player.volume = if (next) 0f else 1f
                    chromeTick += 1
                }.padding(8.dp),
            )
        }
    }

    @Composable
    fun EpisodeBar(pickHere: Boolean) {
        val total = if (episodePaging) {
            episodeTotal.coerceAtLeast(1)
        } else {
            (detail?.episodeCount?.takeIf { it > 0 } ?: Library.get(currentId)?.episodeCount ?: playingEp).coerceAtLeast(1)
        }
        val label = if (episodePaging || pickHere) "选集 · 第 $playingEp 集 · 全 $total 集" else "选集 · 全 $total 集"
        Row(
            Modifier
                .fillMaxWidth()
                .padding(horizontal = 12.dp)
                .clip(RoundedCornerShape(12.dp))
                .background(Color(0x22FFFFFF))
                .clickable {
                    if (pickHere || episodePaging || onEnterSeries == null) sheet = SheetKind.Episodes
                    else onEnterSeries.invoke(currentId, playingEp, player.currentPosition.coerceAtLeast(0L) / 1000.0)
                    chromeTick += 1
                }
                .padding(horizontal = 14.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(Icons.Outlined.List, null, tint = Color.White)
            Spacer(Modifier.width(8.dp))
            Text(label, color = Color.White, fontSize = 14.sp, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
            Icon(
                if (episodePaging || pickHere) Icons.Filled.KeyboardArrowUp else Icons.Filled.KeyboardArrowRight,
                null,
                tint = Color.White,
            )
        }
    }

    @Composable
    fun Status() {
        if (paused && failed.isBlank()) {
            Icon(
                Icons.Filled.PlayArrow,
                "播放",
                tint = Color.White,
                modifier = Modifier.size(72.dp).clickable { player.play() },
            )
        }
        if ((switching || buffering) && !scrubbing && failed.isBlank()) {
            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                CircularProgressIndicator(color = Color(0xFFE08A6E), strokeWidth = 2.dp)
                Spacer(Modifier.height(8.dp))
                Text("加载中", color = Color.White, fontSize = 13.sp)
            }
        }
        if (failed.isNotBlank()) {
            Column(horizontalAlignment = Alignment.CenterHorizontally, modifier = Modifier.padding(24.dp)) {
                Text(failed, color = Color.White, fontSize = 14.sp, textAlign = TextAlign.Center)
                Spacer(Modifier.height(12.dp))
                Text(
                    "重试",
                    color = Color(0xFF2A120C),
                    modifier = Modifier.clip(RoundedCornerShape(12.dp)).background(Color(0xFFE08A6E)).clickable {
                        if (lineAttempt != 0) lineAttempt = 0 else ticket += 1
                    }.padding(horizontal = 16.dp, vertical = 8.dp),
                )
            }
        }
    }

    val title = detail?.title ?: Library.get(currentId)?.title.orEmpty()
    val desc = detail?.description ?: Library.get(currentId)?.description.orEmpty()
    val tags = detail?.tags ?: Library.get(currentId)?.tags.orEmpty()

    Box(Modifier.fillMaxSize().background(Color.Black)) {
        Box(Modifier.fillMaxSize()) {
                VerticalPager(state = pager, userScrollEnabled = !mini && !inPip, modifier = Modifier.fillMaxSize()) { page ->
                    val id = if (episodePaging) currentId else ids.getOrNull(page) ?: currentId
                    val card = Library.get(id)
                    val active = page == pager.currentPage
                    Box(Modifier.fillMaxSize()) {
                        if (active) {
                            AndroidView(
                                factory = { ctx ->
                                    PlayerView(ctx).apply {
                                        useController = false
                                        this.player = player
                                        resizeMode = AspectRatioFrameLayout.RESIZE_MODE_ZOOM
                                        layoutParams = ViewGroup.LayoutParams(
                                            ViewGroup.LayoutParams.MATCH_PARENT,
                                            ViewGroup.LayoutParams.MATCH_PARENT,
                                        )
                                        setShutterBackgroundColor(android.graphics.Color.BLACK)
                                    }
                                },
                                modifier = Modifier.fillMaxSize(),
                                update = {
                                    it.player = player
                                    it.resizeMode = if (fullscreen || mini || inPip || wide) {
                                        AspectRatioFrameLayout.RESIZE_MODE_FIT
                                    } else {
                                        AspectRatioFrameLayout.RESIZE_MODE_ZOOM
                                    }
                                },
                            )
                        } else {
                            CoverImage(card?.cover.orEmpty(), Modifier.fillMaxSize(), 0.dp)
                        }
                        if (!mini && !inPip) Box(
                            Modifier.fillMaxSize().pointerInput(active, fullscreen) {
                                if (!active) return@pointerInput
                                detectTapGestures(onTap = {
                                    if (fullscreen) {
                                        chrome = !chrome
                                        if (chrome) chromeTick += 1
                                    } else if (player.isPlaying) {
                                        player.pause()
                                    } else {
                                        player.play()
                                    }
                                })
                            },
                        )
                    }
                }
                if (!fullscreen && !mini && !inPip) {
                    Column(
                        Modifier.align(Alignment.CenterEnd).padding(end = 10.dp, bottom = 210.dp),
                        verticalArrangement = Arrangement.spacedBy(14.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        val liked = app.store.isLiked(currentId)
                        val faved = app.store.isFav(currentId)
                        RailButton(if (liked) Icons.Filled.Favorite else Icons.Outlined.FavoriteBorder, if (liked) Color(0xFFE08A6E) else Color.White, fmtCount(detail?.like ?: Library.get(currentId)?.likeCount ?: 0)) {
                            val on = app.store.toggleLike(currentId)
                            if (app.store.loggedIn) scope.launch { runCatching { app.api.like(currentId, playingEp, on) } }
                        }
                        RailButton(if (faved) Icons.Filled.Star else Icons.Outlined.StarOutline, if (faved) Color(0xFFE08A6E) else Color.White, "收藏") {
                            val on = app.store.toggleFav(currentId)
                            if (app.store.loggedIn) scope.launch { runCatching { app.api.collect(currentId, on) } }
                            app.store.show(if (on) "已收藏" else "已取消收藏")
                        }
                        val commentLabel = if (detail == null && commentExtra == 0) "评论" else fmtCount((detail?.comment ?: 0) + commentExtra)
                        RailButton(Icons.Outlined.ChatBubbleOutline, Color.White, commentLabel) {
                            sheet = SheetKind.Comments
                            scope.launch {
                                comments.clear()
                                runCatching { app.api.comments(currentId, playingEp, 1) }.onSuccess { comments.addAll(it.first) }
                            }
                        }
                    }
                }
                if (!fullscreen) {
                    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { Status() }
                }
            }
            if (!fullscreen && !mini && !inPip) {
                Box(
                    Modifier.fillMaxWidth().height(220.dp).align(Alignment.BottomCenter).background(
                        Brush.verticalGradient(listOf(Color.Transparent, Color(0xCC000000))),
                    ),
                )
                Column(
                    Modifier.align(Alignment.BottomCenter).then(if (onBack != null) Modifier.navigationBarsPadding() else Modifier),
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    if (!episodePaging) {
                        Column(Modifier.fillMaxWidth().padding(start = 16.dp, end = 84.dp, bottom = 8.dp)) {
                            Text(title, color = Color.White, fontFamily = Serif, fontSize = 22.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
                            if (tags.isNotEmpty()) {
                                Spacer(Modifier.height(8.dp))
                                Text(tags.take(3).joinToString("  ·  "), color = Color(0xFFE7D2C8), fontSize = 12.sp)
                            }
                            if (desc.isNotBlank()) {
                                Spacer(Modifier.height(6.dp))
                                Text(desc.replace('\n', ' '), color = Color(0xD9FFFFFF), fontSize = 13.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
                            }
                        }
                    }
                    ProgressScrubber(player) { scrubbing = it }
                    EpisodeBar(pickHere = false)
                    Spacer(Modifier.height(8.dp))
                }
                TopControls()
            }
        }
        if (fullscreen && chrome && !mini && !inPip) {
            Column(Modifier.fillMaxSize().statusBarsPadding()) {
                TopControls()
                Box(
                    Modifier.weight(1f).fillMaxWidth().pointerInput(Unit) {
                        detectTapGestures { chrome = false }
                    },
                    contentAlignment = Alignment.Center,
                ) {
                    Status()
                }
                ProgressScrubber(player) { active ->
                    scrubbing = active
                    if (active) chromeTick += 1
                }
                EpisodeBar(pickHere = true)
                Spacer(Modifier.navigationBarsPadding().height(8.dp))
            }
        }

    if (sheet != null) {
        ModalBottomSheet(
            onDismissRequest = { sheet = null },
            sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
            containerColor = LocalHg.current.paper,
        ) {
            when (sheet) {
                SheetKind.Episodes -> EpisodeSheet(detail, playingEp) { next ->
                    sheet = null
                    resume = 0.0
                    if (episodePaging) {
                        scope.launch {
                            pager.scrollToPage((next - 1).coerceIn(0, (episodeTotal - 1).coerceAtLeast(0)))
                        }
                    } else {
                        picked = true
                        ep = next
                        val prev = app.store.history.firstOrNull { it.id == currentId }
                        val t = if (prev?.ep == next) prev.t else 0.0
                        app.store.saveWatch(currentId, next, t, prev?.dur ?: 0.0)
                    }
                }
                SheetKind.Comments -> CommentSheet(comments, commentText, { commentText = it }) {
                    val text = commentText.trim()
                    if (text.isEmpty()) return@CommentSheet
                    if (!app.store.loggedIn) {
                        app.store.show("登录后才能评论")
                        return@CommentSheet
                    }
                    scope.launch {
                        runCatching { app.api.sendComment(currentId, playingEp, text) }
                            .onSuccess {
                                commentText = ""
                                commentExtra += 1
                                app.store.show("已发送")
                                runCatching { app.api.comments(currentId, playingEp, 1) }.onSuccess {
                                    comments.clear()
                                    comments.addAll(it.first)
                                }
                            }
                            .onFailure { app.store.show(it.message ?: "发送失败") }
                    }
                }
                null -> Unit
            }
            Spacer(Modifier.navigationBarsPadding())
        }
    }
}

private enum class SheetKind { Episodes, Comments }

private fun Context.findActivity(): Activity? {
    var current: Context = this
    while (current is ContextWrapper) {
        if (current is Activity) return current
        current = current.baseContext
    }
    return null
}

private const val HgApiUa =
    "Mozilla/5.0 (Linux; Android 14; Mobile) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36"

private class EndBridge {
    var go: () -> Unit = {}
    var at: Long = 0L
}

private fun playRetryable(message: String): Boolean {
    val msg = message.lowercase()
    return msg.contains("http 408") || msg.contains("http 429") || msg.contains("http 5") ||
        msg.contains("timeout") || msg.contains("timed out") || msg.contains("超时") ||
        msg.contains("failed to connect") || msg.contains("unable to resolve")
}

private fun stillCurrent(episodePaging: Boolean, page: Int, ids: List<Int>, id: Int, episode: Int): Boolean {
    return if (episodePaging) page + 1 == episode else ids.getOrNull(page) == id
}

@Composable
private fun ProgressScrubber(player: Player, onActive: (Boolean) -> Unit = {}) {
    var label by remember { mutableStateOf<String?>(null) }
    var dragMs by remember { mutableStateOf<Long?>(null) }
    var clock by remember { mutableStateOf(0L to 0L) }
    val shownPos = dragMs ?: clock.first
    val shownDur = clock.second
    Column(Modifier.fillMaxWidth().padding(horizontal = 12.dp)) {
        Box(Modifier.fillMaxWidth().height(18.dp), contentAlignment = Alignment.Center) {
            if (label != null) {
                Text(label!!, color = Color.White, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
            }
        }
        AndroidView(
            factory = { ctx ->
                ThickSeekBar(ctx).apply {
                    setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
                        override fun onProgressChanged(seekBar: SeekBar, progress: Int, fromUser: Boolean) {
                            if (!fromUser) return
                            val bar = seekBar as ThickSeekBar
                            val duration = bar.player?.duration ?: 0L
                            if (duration <= 0) return
                            val target = duration * progress / seekBar.max
                            dragMs = target
                            label = fmtClock(target / 1000.0)
                            onActive(true)
                            val now = SystemClock.uptimeMillis()
                            if (now - bar.lastSeekMs >= 40) {
                                bar.lastSeekMs = now
                                bar.player?.seekTo(target)
                            }
                        }

                        override fun onStartTrackingTouch(seekBar: SeekBar) {
                            (seekBar as ThickSeekBar).tracking = true
                            seekBar.parent?.requestDisallowInterceptTouchEvent(true)
                            onActive(true)
                        }

                        override fun onStopTrackingTouch(seekBar: SeekBar) {
                            val bar = seekBar as ThickSeekBar
                            bar.tracking = false
                            val duration = bar.player?.duration ?: 0L
                            if (duration > 0) bar.player?.seekTo(duration * seekBar.progress / seekBar.max)
                            dragMs = null
                            label = null
                            onActive(false)
                        }
                    })
                }
            },
            modifier = Modifier.fillMaxWidth().height(36.dp),
            update = {
                it.player = player
                it.onClock = { position, duration -> clock = position to duration }
            },
        )
        Text(
            "${fmtClock(shownPos / 1000.0)} / ${fmtClock(shownDur / 1000.0)}",
            color = Color.White.copy(alpha = 0.9f),
            fontSize = 12.sp,
            modifier = Modifier.fillMaxWidth().padding(top = 2.dp),
            textAlign = TextAlign.Center,
        )
    }
}

/** 系统 SeekBar：圆点跟手，播放进度按帧前进，不跟着整页重组。 */
private class ThickSeekBar(context: android.content.Context) : SeekBar(context) {
    var player: Player? = null
    var tracking = false
    var lastSeekMs = 0L
    var onClock: ((Long, Long) -> Unit)? = null
    private var lastClockMs = 0L

    private val frame = object : Choreographer.FrameCallback {
        override fun doFrame(frameTimeNanos: Long) {
            val current = player
            if (current != null) {
                val duration = current.duration
                val position = current.currentPosition.coerceAtLeast(0L)
                val now = SystemClock.uptimeMillis()
                if (now - lastClockMs >= 200) {
                    lastClockMs = now
                    onClock?.invoke(position, if (duration > 0) duration else 0L)
                }
                if (!tracking && duration > 0) {
                    val next = ((position.toDouble() / duration) * max).toInt().coerceIn(0, max)
                    if (progress != next) progress = next
                }
            }
            Choreographer.getInstance().postFrameCallback(this)
        }
    }

    init {
        max = 1000
        splitTrack = false
        isDuplicateParentStateEnabled = false
        val trackHeight = (10 * resources.displayMetrics.density).toInt().coerceAtLeast(8)
        val thumbSize = (24 * resources.displayMetrics.density).toInt()
        val background = GradientDrawable().apply {
            shape = GradientDrawable.RECTANGLE
            cornerRadius = trackHeight / 2f
            setColor(0x66FFFFFF)
            setSize(trackHeight, trackHeight)
        }
        val played = GradientDrawable().apply {
            shape = GradientDrawable.RECTANGLE
            cornerRadius = trackHeight / 2f
            setColor(0xFFE08A6E.toInt())
            setSize(trackHeight, trackHeight)
        }
        val layers = LayerDrawable(arrayOf(background, ClipDrawable(played, Gravity.START, ClipDrawable.HORIZONTAL)))
        layers.setId(0, android.R.id.background)
        layers.setId(1, android.R.id.progress)
        val inset = (15 * resources.displayMetrics.density).toInt()
        progressDrawable = InsetDrawable(layers, 0, inset, 0, inset)
        thumb = GradientDrawable().apply {
            shape = GradientDrawable.OVAL
            setColor(0xFFFFFFFF.toInt())
            setSize(thumbSize, thumbSize)
        }
        thumbOffset = thumbSize / 2
        val pad = thumbSize / 2
        setPadding(pad, 0, pad, 0)
        minimumHeight = thumbSize
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        Choreographer.getInstance().postFrameCallback(frame)
    }

    override fun onDetachedFromWindow() {
        Choreographer.getInstance().removeFrameCallback(frame)
        super.onDetachedFromWindow()
    }
}

@Composable
private fun QualityMenu(
    preferHevc: Boolean,
    hevcReady: Boolean,
    known: Boolean,
    onPick: (Boolean) -> Unit,
) {
    var open by remember { mutableStateOf(false) }
    val playingHevc = preferHevc && (hevcReady || !known)
    Box {
        Text(
            if (playingHevc) "高清" else "标准",
            color = Color.White,
            fontSize = 13.sp,
            modifier = Modifier
                .clip(RoundedCornerShape(999.dp))
                .background(Color(0x66000000))
                .clickable { open = true }
                .padding(horizontal = 12.dp, vertical = 6.dp),
        )
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            DropdownMenuItem(
                text = { Text(if (playingHevc) "标准" else "标准  ✓") },
                onClick = {
                    open = false
                    onPick(false)
                },
            )
            DropdownMenuItem(
                text = {
                    Text(
                        when {
                            known && !hevcReady -> "高清（这一集没有）"
                            playingHevc -> "高清  ✓"
                            else -> "高清"
                        },
                    )
                },
                onClick = {
                    open = false
                    onPick(true)
                },
            )
        }
    }
}

@Composable
private fun RailButton(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    tint: Color,
    label: String,
    onClick: () -> Unit,
) {
    Column(horizontalAlignment = Alignment.CenterHorizontally, modifier = Modifier.clickable { onClick() }) {
        Icon(icon, label, tint = tint, modifier = Modifier.size(28.dp))
        Text(label, color = Color.White, fontSize = 11.sp)
    }
}

@Composable
private fun EpisodeSheet(detail: Detail?, current: Int, onPick: (Int) -> Unit) {
    val c = LocalHg.current
    val eps = detail?.episodes.orEmpty()
    Column(Modifier.fillMaxWidth().padding(horizontal = 20.dp)) {
        Text(detail?.title ?: "选集", color = c.ink, fontFamily = Serif, fontSize = 22.sp)
        Spacer(Modifier.height(12.dp))
        if (eps.isEmpty()) {
            Text("选集加载中", color = c.muted)
        } else {
            LazyColumn(Modifier.height(360.dp)) {
                items(eps, key = { it.ep }) { e ->
                    val on = e.ep == current
                    Row(
                        Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).background(if (on) c.soft else Color.Transparent)
                            .clickable { onPick(e.ep) }.padding(horizontal = 12.dp, vertical = 12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(if (on) "正在播放" else "第 ${e.ep} 集", color = if (on) c.accent else c.muted, fontSize = 12.sp, modifier = Modifier.width(72.dp))
                        Text(e.title, color = c.ink, fontSize = 15.sp, modifier = Modifier.weight(1f))
                        if (on) Icon(Icons.Filled.Pause, null, tint = c.accent, modifier = Modifier.size(16.dp))
                    }
                }
            }
        }
        Spacer(Modifier.height(12.dp))
    }
}

@Composable
private fun CommentSheet(
    comments: List<Comment>,
    draft: String,
    onDraft: (String) -> Unit,
    onSend: () -> Unit,
) {
    val c = LocalHg.current
    Column(Modifier.fillMaxWidth().padding(horizontal = 20.dp)) {
        Text("评论", color = c.ink, fontFamily = Serif, fontSize = 22.sp)
        Spacer(Modifier.height(8.dp))
        LazyColumn(Modifier.height(280.dp)) {
            if (comments.isEmpty()) {
                item { Text("还没有评论", color = c.muted, modifier = Modifier.padding(vertical = 24.dp)) }
            }
            items(comments, key = { it.id }) { item ->
                Column(Modifier.fillMaxWidth().padding(vertical = 8.dp)) {
                    Text(item.nickname, color = c.ink, fontWeight = FontWeight.SemiBold, fontSize = 13.sp)
                    Text(item.text, color = c.ink, fontSize = 14.sp)
                    Text(item.time, color = c.faint, fontSize = 11.sp)
                }
            }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            TextField(
                value = draft,
                onValueChange = onDraft,
                placeholder = { Text("说点什么") },
                modifier = Modifier.weight(1f),
                colors = TextFieldDefaults.colors(
                    focusedContainerColor = c.card,
                    unfocusedContainerColor = c.card,
                    focusedIndicatorColor = Color.Transparent,
                    unfocusedIndicatorColor = Color.Transparent,
                ),
            )
            Spacer(Modifier.width(8.dp))
            Text(
                "发送",
                color = c.onAccent,
                modifier = Modifier.clip(RoundedCornerShape(12.dp)).background(c.accent).clickable { onSend() }.padding(horizontal = 14.dp, vertical = 10.dp),
            )
        }
        Spacer(Modifier.height(8.dp))
    }
}

@Composable
fun HomeFeed(
    onOpenSearch: () -> Unit,
    onEnterSeries: (Int, Int, Double) -> Unit = { _, _, _ -> },
    mini: Boolean = false,
    onMini: ((Int, Int, Double) -> Unit)? = null,
) {
    val app = LocalApp.current
    val scope = rememberCoroutineScope()
    var items by remember { mutableStateOf(HomeCache.items) }
    var page by remember { mutableIntStateOf(HomeCache.page) }
    var done by remember { mutableStateOf(HomeCache.done) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf("") }

    suspend fun load(next: Int) {
        if (loading || done && next != 1) return
        loading = true
        error = ""
        val result = runCatching { app.api.feed(next, 8) }
        loading = false
        result.onSuccess { (list, info) ->
            Library.put(list)
            items = if (next == 1) list else (items + list).distinctBy { it.id }
            page = next
            done = list.isEmpty() || next >= info.pages
            HomeCache.items = items
            HomeCache.page = page
            HomeCache.done = done
        }.onFailure {
            error = it.message ?: "首页加载失败"
            if (items.isEmpty()) app.store.show(error)
        }
    }

    LaunchedEffect(Unit) { if (items.isEmpty()) load(1) }

    when {
        items.isEmpty() && loading -> CenterSpinner()
        items.isEmpty() -> EmptyHint("首页加载失败", error.ifBlank { "请检查网络或切换线路" }, "重试") {
            done = false
            scope.launch { load(1) }
        }
        else -> VideoFeed(
            ids = items.map { it.id },
            start = 0,
            startEp = 1,
            startTimeSec = 0.0,
            onBack = null,
            onNearEnd = {
                if (!loading && !done) scope.launch { load(page + 1) }
            },
            onEnterSeries = onEnterSeries,
            mini = mini,
            onMini = onMini,
        )
    }
    if (false) onOpenSearch()
}

private object HomeCache {
    var items: List<app.hghub.mobile.data.VideoCard> = emptyList()
    var page: Int = 1
    var done: Boolean = false
}

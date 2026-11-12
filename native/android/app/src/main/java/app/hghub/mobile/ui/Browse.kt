package app.hghub.mobile.ui

import app.hghub.mobile.data.safeHttp

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import app.hghub.mobile.Library
import app.hghub.mobile.LocalApp
import app.hghub.mobile.data.Post
import app.hghub.mobile.data.RankItem
import app.hghub.mobile.data.VideoCard
import app.hghub.mobile.data.fmtCount
import kotlinx.coroutines.launch

val Channels = listOf(
    "ai-duanju" to "AI成人短剧",
    "ai-manju" to "AI成人漫剧",
    "ai-huanlian" to "AI换脸",
    "ai-mogai" to "AI魔改",
)

private val HotTags = listOf("都市", "逆袭", "古风", "系统", "校园", "仙侠", "职场", "脑洞", "奇幻")

@Composable
fun SearchScreen(onOpen: (List<Int>, Int) -> Unit, onRanks: () -> Unit) {
    val app = LocalApp.current
    val scope = rememberCoroutineScope()
    val c = LocalHg.current
    var q by remember { mutableStateOf("") }
    var submitted by remember { mutableStateOf("") }
    var items by remember { mutableStateOf<List<VideoCard>>(emptyList()) }
    var page by remember { mutableIntStateOf(1) }
    var pages by remember { mutableIntStateOf(1) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf("") }

    fun run(keyword: String, next: Int) {
        scope.launch {
            loading = true
            val result = runCatching { app.api.search(keyword, next, 20) }
            loading = false
            result.onSuccess { (list, info) ->
                Library.put(list)
                items = if (next == 1) list else (items + list).distinctBy { it.id }
                page = next
                pages = info.pages
                error = ""
            }.onFailure { error = it.message ?: "搜索失败" }
        }
    }

    Column(Modifier.fillMaxSize().background(c.paper)) {
        PageTitle("搜索", "短剧、作者、题材")
        Row(
            Modifier.padding(horizontal = 20.dp).fillMaxWidth().clip(RoundedCornerShape(16.dp)).background(c.card).padding(horizontal = 14.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            androidx.compose.material3.Icon(Icons.Filled.Search, null, tint = c.faint)
            Spacer(Modifier.width(8.dp))
            BasicTextField(
                value = q,
                onValueChange = { q = it },
                textStyle = TextStyle(color = c.ink, fontSize = 15.sp),
                modifier = Modifier.weight(1f),
                singleLine = true,
                decorationBox = { inner ->
                    if (q.isEmpty()) Text("搜索", color = c.faint, fontSize = 15.sp)
                    inner()
                },
            )
            Text("搜索", color = c.accent, fontWeight = FontWeight.SemiBold, modifier = Modifier.clickable {
                val k = q.trim()
                if (k.isEmpty()) return@clickable
                submitted = k
                app.store.rememberSearch(k)
                run(k, 1)
            })
        }
        if (submitted.isEmpty()) {
            LazyColumn(contentPadding = PaddingValues(20.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                if (app.store.searches.isNotEmpty()) {
                    item {
                        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                            Text("最近", color = c.ink, fontFamily = Serif, fontSize = 18.sp)
                            Text("清空", color = c.faint, fontSize = 13.sp, modifier = Modifier.clickable { app.store.clearSearch() })
                        }
                    }
                    item {
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.horizontalScroll(rememberScrollState())) {
                            app.store.searches.forEach { h -> Chip(h) { q = h; submitted = h; app.store.rememberSearch(h); run(h, 1) } }
                        }
                    }
                }
                item { Text("热门题材", color = c.ink, fontFamily = Serif, fontSize = 18.sp) }
                item {
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.horizontalScroll(rememberScrollState())) {
                        HotTags.forEach { tag -> Chip(tag) { q = tag; submitted = tag; app.store.rememberSearch(tag); run(tag, 1) } }
                    }
                }
                item {
                    Text("查看排行榜", color = c.onAccent, modifier = Modifier.clip(RoundedCornerShape(14.dp)).background(c.accent).clickable { onRanks() }.padding(horizontal = 16.dp, vertical = 10.dp))
                }
            }
        } else {
            if (error.isNotBlank() && items.isEmpty()) {
                EmptyHint("没有结果", error, "重试") { run(submitted, 1) }
            } else {
                LazyColumn(contentPadding = PaddingValues(horizontal = 20.dp, vertical = 8.dp)) {
                    items(items, key = { it.id }) { v ->
                        VideoRow(v.title, v.cover, metaOf(v)) {
                            onOpen(items.map { it.id }, items.indexOfFirst { it.id == v.id }.coerceAtLeast(0))
                        }
                    }
                    item {
                        if (page < pages) {
                            Text(
                                if (loading) "加载中…" else "更多",
                                color = c.accent,
                                modifier = Modifier.fillMaxWidth().clickable(enabled = !loading) { run(submitted, page + 1) }.padding(vertical = 16.dp),
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
fun CategoryScreen(onChannel: (String, String) -> Unit, onRanks: () -> Unit, onPosts: () -> Unit, onOpen: (List<Int>, Int) -> Unit) {
    val app = LocalApp.current
    val c = LocalHg.current
    var rows by remember { mutableStateOf<Map<String, List<VideoCard>>>(emptyMap()) }
    LaunchedEffect(Unit) {
        val next = rows.toMutableMap()
        for ((cate, _) in Channels) {
            if (next[cate].isNullOrEmpty()) {
                runCatching { app.api.category(cate, 1, 8) }.onSuccess { (list, _) ->
                    Library.put(list)
                    next[cate] = list
                    rows = next.toMap()
                }
            }
        }
    }
    LazyColumn(Modifier.fillMaxSize().background(c.paper)) {
        item { PageTitle("分类", "四个频道，持续更新") }
        item {
            Row(
                Modifier.padding(horizontal = 20.dp, vertical = 6.dp).fillMaxWidth().clip(RoundedCornerShape(18.dp)).background(c.card).clickable { onRanks() }.padding(16.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text("排行榜", color = c.ink, fontFamily = Serif, fontSize = 18.sp)
                    Text("热播 · 推荐 · 潜力", color = c.muted, fontSize = 12.sp)
                }
                Text("打开", color = c.accent, fontSize = 13.sp)
            }
        }
        items(Channels) { (cate, name) ->
            Column(Modifier.padding(top = 16.dp)) {
                Row(Modifier.fillMaxWidth().clickable { onChannel(cate, name) }.padding(horizontal = 20.dp), horizontalArrangement = Arrangement.SpaceBetween) {
                    Text(name, color = c.ink, fontFamily = Serif, fontSize = 20.sp)
                    Text("全部", color = c.muted, fontSize = 13.sp)
                }
                Spacer(Modifier.height(10.dp))
                Row(Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 20.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    val list = rows[cate].orEmpty()
                    if (list.isEmpty()) Text("加载中", color = c.faint, modifier = Modifier.padding(vertical = 28.dp))
                    list.forEach { v ->
                        Column(Modifier.width(112.dp).clickable {
                            onOpen(list.map { it.id }, list.indexOfFirst { it.id == v.id }.coerceAtLeast(0))
                        }) {
                            CoverImage(v.cover, Modifier.width(112.dp).height(150.dp), 14.dp)
                            Spacer(Modifier.height(6.dp))
                            Text(v.title, color = c.ink, fontSize = 13.sp, maxLines = 2)
                        }
                    }
                }
            }
        }
        item {
            Row(
                Modifier.padding(20.dp).fillMaxWidth().clip(RoundedCornerShape(18.dp)).background(c.soft).clickable { onPosts() }.padding(16.dp),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Column {
                    Text("黄果吃瓜", color = c.ink, fontFamily = Serif, fontSize = 20.sp)
                    Text("圈内更新", color = c.muted, fontSize = 12.sp)
                }
                Text("进入", color = c.accent)
            }
        }
        item { Spacer(Modifier.height(24.dp)) }
    }
}

@Composable
fun ChannelScreen(cate: String, name: String, onBack: () -> Unit, onOpen: (List<Int>, Int) -> Unit) {
    val app = LocalApp.current
    val c = LocalHg.current
    var items by remember { mutableStateOf<List<VideoCard>>(emptyList()) }
    var page by remember { mutableIntStateOf(0) }
    var pages by remember { mutableIntStateOf(1) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf("") }
    val scope = rememberCoroutineScope()

    fun load(next: Int) {
        if (loading) return
        scope.launch {
            loading = true
            val result = runCatching { app.api.category(cate, next, 20) }
            loading = false
            result.onSuccess { (list, info) ->
                Library.put(list)
                items = if (next == 1) list else (items + list).distinctBy { it.id }
                page = next
                pages = info.pages
                error = ""
            }.onFailure { error = it.message ?: "加载失败" }
        }
    }
    LaunchedEffect(cate) { load(1) }

    Column(Modifier.fillMaxSize().background(c.paper)) {
        PageTitle(name, null, onBack)
        when {
            items.isEmpty() && loading -> CenterSpinner()
            items.isEmpty() -> EmptyHint("这个频道是空的", error.ifBlank { "稍后再来" }, "重试") { load(1) }
            else -> LazyColumn(contentPadding = PaddingValues(horizontal = 20.dp)) {
                items(items, key = { it.id }) { v ->
                    VideoRow(v.title, v.cover, metaOf(v)) {
                        onOpen(items.map { it.id }, items.indexOfFirst { it.id == v.id }.coerceAtLeast(0))
                    }
                }
                item {
                    if (page < pages) {
                        Text(if (loading) "加载中…" else "更多", color = c.accent, modifier = Modifier.clickable(enabled = !loading) { load(page + 1) }.padding(vertical = 16.dp))
                    }
                }
            }
        }
    }
}

@Composable
fun RankScreen(onBack: () -> Unit, onOpen: (Int) -> Unit) {
    val app = LocalApp.current
    val c = LocalHg.current
    val kinds = listOf("hot" to "热播", "recommend" to "推荐", "potential" to "潜力")
    var kind by remember { mutableStateOf("hot") }
    var items by remember { mutableStateOf<List<RankItem>>(emptyList()) }
    var error by remember { mutableStateOf("") }
    LaunchedEffect(kind) {
        error = ""
        items = emptyList()
        runCatching { app.api.rank(kind) }.onSuccess { items = it }.onFailure { error = it.message ?: "榜单加载失败" }
    }
    Column(Modifier.fillMaxSize().background(c.paper)) {
        PageTitle("排行榜", null, onBack)
        Row(Modifier.padding(horizontal = 20.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            kinds.forEach { (k, label) ->
                val on = k == kind
                Text(
                    label,
                    color = if (on) c.onAccent else c.ink,
                    modifier = Modifier.clip(RoundedCornerShape(999.dp)).background(if (on) c.accent else c.card).clickable { kind = k }.padding(horizontal = 14.dp, vertical = 8.dp),
                )
            }
        }
        when {
            error.isNotBlank() && items.isEmpty() -> EmptyHint("榜单加载失败", error, "重试") { kind = kind }
            items.isEmpty() -> CenterSpinner()
            else -> LazyColumn(contentPadding = PaddingValues(20.dp)) {
                items(items, key = { it.videoId }) { r ->
                    VideoRow(r.title, r.cover, "${r.rank}  ·  ${r.metricLabel} ${fmtCount(r.metricValue)}") {
                        onOpen(r.videoId)
                    }
                }
            }
        }
    }
}

@Composable
fun PostsScreen(onBack: () -> Unit, onOpen: (Int) -> Unit) {
    val app = LocalApp.current
    val c = LocalHg.current
    var items by remember { mutableStateOf<List<Post>>(emptyList()) }
    var page by remember { mutableIntStateOf(0) }
    var pages by remember { mutableIntStateOf(1) }
    var loading by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    fun load(next: Int) {
        if (loading) return
        scope.launch {
            loading = true
            runCatching { app.api.posts(next, 20) }.onSuccess { (list, info) ->
                items = if (next == 1) list else (items + list).distinctBy { it.id }
                page = next
                pages = info.pages
            }
            loading = false
        }
    }
    LaunchedEffect(Unit) { load(1) }
    Column(Modifier.fillMaxSize().background(c.paper)) {
        PageTitle("吃瓜", "每日更新", onBack)
        if (items.isEmpty()) CenterSpinner() else {
            LazyVerticalGrid(columns = GridCells.Fixed(2), contentPadding = PaddingValues(16.dp), horizontalArrangement = Arrangement.spacedBy(10.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                items(items, key = { it.id }) { p ->
                    Column(Modifier.clip(RoundedCornerShape(16.dp)).background(c.card).clickable { onOpen(p.id) }.padding(8.dp)) {
                        CoverImage(p.cover, Modifier.fillMaxWidth().height(120.dp), 12.dp)
                        Spacer(Modifier.height(8.dp))
                        Text(p.title, color = c.ink, fontSize = 14.sp, maxLines = 2)
                    }
                }
                item {
                    if (page < pages) {
                        Text(if (loading) "…" else "更多", color = c.accent, modifier = Modifier.clickable { load(page + 1) }.padding(12.dp))
                    }
                }
            }
        }
    }
}

@Composable
fun PostScreen(id: Int, onBack: () -> Unit) {
    val app = LocalApp.current
    val c = LocalHg.current
    var post by remember { mutableStateOf<Post?>(null) }
    var error by remember { mutableStateOf("") }
    LaunchedEffect(id) {
        runCatching { app.api.post(id) }.onSuccess { post = it }.onFailure { error = it.message ?: "加载失败" }
    }
    Column(Modifier.fillMaxSize().background(c.paper)) {
        PageTitle(post?.title ?: "正文", null, onBack)
        val p = post
        when {
            p == null && error.isBlank() -> CenterSpinner()
            p == null -> EmptyHint("打不开这篇", error, null, null)
            else -> LazyColumn(contentPadding = PaddingValues(20.dp)) {
                item { CoverImage(p.cover, Modifier.fillMaxWidth().height(190.dp), 16.dp) }
                item { Spacer(Modifier.height(14.dp)) }
                if (p.videoUrl.isNotBlank()) {
                    item {
                        Text("这篇带视频，点下面播放", color = c.muted, fontSize = 13.sp)
                        Spacer(Modifier.height(8.dp))
                        InlineClip(p.videoUrl)
                        Spacer(Modifier.height(14.dp))
                    }
                }
                item {
                    val plain = p.content
                        .replace(Regex("(?is)<script.*?>.*?</script>"), " ")
                        .replace(Regex("(?is)<style.*?>.*?</style>"), " ")
                        .replace(Regex("<[^>]+>"), " ")
                        .replace("&nbsp;", " ")
                        .replace(Regex("\\s+"), " ")
                        .trim()
                    Text(plain.ifBlank { p.desc.ifBlank { "没有正文" } }, color = c.ink, fontSize = 15.sp, lineHeight = 24.sp)
                }
            }
        }
    }
}

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@Composable
private fun InlineClip(url: String) {
    val app = LocalApp.current
    val context = androidx.compose.ui.platform.LocalContext.current
    val base = app.store.base
    val player = remember(url, base) {
        val headers = mapOf("Referer" to base.trimEnd('/') + "/", "Origin" to base.trimEnd('/'))
        val dataSource = androidx.media3.datasource.okhttp.OkHttpDataSource.Factory(safeHttp)
            .setUserAgent("Mozilla/5.0 (Linux; Android 14; Mobile) AppleWebKit/537.36 Chrome/131.0.0.0 Mobile Safari/537.36")
            .setDefaultRequestProperties(headers)
        androidx.media3.exoplayer.ExoPlayer.Builder(context)
            .setMediaSourceFactory(androidx.media3.exoplayer.source.DefaultMediaSourceFactory(dataSource))
            .build().apply {
                setMediaItem(androidx.media3.common.MediaItem.fromUri(url))
                volume = if (app.store.muted) 0f else 1f
                prepare()
                playWhenReady = true
            }
    }
    androidx.compose.runtime.DisposableEffect(player) { onDispose { player.release() } }
    androidx.compose.ui.viewinterop.AndroidView(
        factory = { ctx ->
            androidx.media3.ui.PlayerView(ctx).apply {
                useController = true
                this.player = player
            }
        },
        modifier = Modifier.fillMaxWidth().height(220.dp).clip(RoundedCornerShape(16.dp)),
    )
}

private fun metaOf(v: VideoCard): String {
    val ep = if (v.episodeCount > 0) "${v.episodeCount} 集" else ""
    val hot = if (v.hot > 0) "热度 ${fmtCount(v.hot)}" else ""
    return listOf(ep, hot, v.author?.name.orEmpty()).filter { it.isNotBlank() }.joinToString("  ·  ")
}

import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import Feed from "../src/components/Feed";
import { AppProvider, useStore, type WatchProgress } from "../src/store";
import { api } from "../src/api";
import "../src/index.css";
import { flushPlaybackLog, initPlaybackLog, playbackLogApi, setPlaybackLog } from "../src/playbackDiagnostics";
import Settings from "../src/pages/Settings";
import Player from "../src/components/Player";
import Hls from "hls.js";
import Category, { ChannelPage } from "../src/pages/Category";
import Profile from "../src/pages/Profile";
import Search from "../src/pages/Search";

// 仅将自定义媒体协议映射到本地测试服务器；播放器、HLS 和鼠标事件均使用真实实现。
const proxy = new URLSearchParams(location.search).get("backend") || new URLSearchParams(location.search).get("proxy") || "http://127.0.0.1:17779";
const backend = new URLSearchParams(location.search).get("backend");
const upstream = new URLSearchParams(location.search).get("upstream");
const open = XMLHttpRequest.prototype.open;
XMLHttpRequest.prototype.open = function (this: XMLHttpRequest, method: string, target: string | URL, async: boolean = true, user?: string | null, credential?: string | null) {
  const url = new URL(String(target), location.href);
  if (url.hostname === "hg.localhost" || (url.hostname === "localhost" && url.protocol === "hg:")) {
    target = `${proxy}${url.pathname}${url.search}`;
  }
  return open.call(this, method, target, async, user, credential);
};

const fixture = {
  seeks: [] as number[], errors: [] as string[],
  setActive: (_active: boolean) => {},
  nextSource: () => {},
  sourceKey: 0,
  playRequests: [] as { id: number; ep: number }[],
  failEpisode: 0,
  getProgress: (_id: number): WatchProgress | undefined => undefined,
  record: (_id: number, _ep: number, _time: number, _duration: number, _writer?: number) => {},
  claim: (_id: number) => 0,
  clearHistory: () => {},
  logs: [] as { event: string; data: Record<string, unknown> }[],
  setLogging: setPlaybackLog,
  flushLogs: flushPlaybackLog,
  rejectLoggingSave: false,
  pauseBuffering: () => testHls?.pauseBuffering(),
  resumeBuffering: () => testHls?.resumeBuffering(),
  engineState: () => ({ loading: testHls?.loadingEnabled, buffering: testHls?.bufferingEnabled, levels: testHls?.levels.length, subtitleTrack: testHls?.subtitleTrack, subtitleCount: testHls?.subtitleTracks.length, subtitleDisplay: testHls?.subtitleDisplay }),
  cacheWrites: [] as { url: string; time: number }[],
  primeCache: (fraction: number) => {
    const video = document.querySelector("video")!;
    const fragments = testHls?.levels[testHls.loadLevel]?.details?.fragments ?? [];
    const index = fragments.findIndex(fragment => fragment.start + fragment.duration > video.duration * fraction);
    if (index < 0 || index + 7 >= fragments.length) throw new Error("Missing fixture fragments");
    const url = (offset: number) => new URL(fragments[index + offset].url).searchParams.get("u")!;
    pendingCache = [url(0), url(1), url(2), url(3), url(6), url(7)];
    return { url: url(4), edge: fragments[index + 4].start };
  },
};
let testHls: Hls | undefined;
let pendingCache: string[] | undefined;
let diskCache = new Set<string>();
let cacheRevision = 0;
const diagnosticTest = new URLSearchParams(location.search).has("diagnostics");
const cacheSeekTest = new URLSearchParams(location.search).has("cache_seek");
const legacyTest = new URLSearchParams(location.search).has("legacy");
const initiallyInactive = new URLSearchParams(location.search).has("inactive");
const subtitleHls = new URLSearchParams(location.search).has("subtitle_hls");
const resumeTest = new URLSearchParams(location.search).has("resume");
const mixedDuration = new URLSearchParams(location.search).has("mixed");
const directTest = new URLSearchParams(location.search).has("direct");
const mediaSource = () => backend ? `${upstream}/vod/${directTest ? 'video.mp4' : 'list.m3u8'}`
  : `https://media.fixture.invalid/${subtitleHls ? "master" : "list"}.m3u8${fixture.sourceKey ? `?source=${fixture.sourceKey}` : ""}`;
if (diagnosticTest) {
  // Test-only handle: exercise the real engine's public API without rewriting production modules.
  const attach = Hls.prototype.attachMedia;
  Hls.prototype.attachMedia = function (...args) {
    testHls = this;
    if (cacheSeekTest) this.config.maxBufferLength = this.config.maxMaxBufferLength = 6;
    return attach.apply(this, args);
  };
  let enabled = false;
  playbackLogApi.status = async () => ({ enabled, path: "playback.log", error: null });
  playbackLogApi.set = async (on) => {
    if (fixture.rejectLoggingSave) throw new Error("fixture: disk unavailable");
    return { enabled: enabled = on, path: "playback.log", error: null };
  };
  playbackLogApi.write = async entries => {
    if (!enabled) return;
    fixture.logs.push(...entries);
    const collect = (window as typeof window & { collectPlaybackLogs?: (entries: typeof fixture.logs) => Promise<void> }).collectPlaybackLogs;
    await collect?.(entries);
  };
  playbackLogApi.read = async () => fixture.logs.map(e => JSON.stringify(e)).join("\n");
  api.lineState = async () => ({ base: "https://fixture.invalid", default: "https://fixture.invalid", fixed: [] });
  void initPlaybackLog();
}
Object.assign(window, { playerSeekFixture: fixture });
document.addEventListener("seeking", (e) => {
  if (!(e.target instanceof HTMLVideoElement)) return;
  fixture.seeks.push(e.target.currentTime);
  if (pendingCache) {
    diskCache = new Set(pendingCache);
    pendingCache = undefined;
    cacheRevision++;
    fixture.cacheWrites.length = 0;
  }
}, true);
window.addEventListener("error", (e) => fixture.errors.push(e.message));
api.playbackCacheStatus = async () => ({ max_mib: cacheRevision ? 256 : 0, bytes: diskCache.size, revision: cacheRevision });
if (cacheSeekTest) {
  api.playbackCacheStart = async () => {};
  api.playbackCacheStop = async () => {};
  api.playbackCacheReady = async () => {};
  api.playbackCacheContains = async keys => keys.map(key => diskCache.has(key));
  api.playbackCachePrefetch = async (url, protectedKeys) => {
    if (diskCache.has(url)) return true;
    if (diskCache.size >= 6) {
      const victim = [...diskCache].find(key => !protectedKeys.includes(key));
      if (!victim) return false;
      diskCache.delete(victim);
    }
    diskCache.add(url);
    fixture.cacheWrites.push({ url, time: document.querySelector("video")!.currentTime });
    return true;
  };
}
if (backend) {
  const command = async <T,>(name: string, args: object = {}): Promise<T> => {
    const response = await fetch(`${backend}/api/${name}`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(args) });
    const result = await response.json();
    if (!response.ok) throw new Error(result.error);
    return result;
  };
  api.playbackCacheStatus = () => command('status');
  api.playbackCacheSet = async maxMib => { await command('set', { maxMib }); window.dispatchEvent(new Event('playback-cache-change')); };
  api.playbackCacheStart = sessionId => command('start', { sessionId });
  api.playbackCacheStop = sessionId => command('stop', { sessionId });
  api.playbackCacheReady = (sessionId, ready) => command('ready', { sessionId, ready });
  api.playbackCacheContains = urls => command('contains', { urls });
  api.playbackCachePrefetch = async (url, protectedUrls, sessionId, range, validator, focus) => {
    const saved = await command<boolean>('prefetch', { url, protectedUrls, sessionId, range, validator, focus });
    if (saved) fixture.cacheWrites.push({ url, time: document.querySelector('video')?.currentTime ?? 0 });
    return saved;
  };
  api.playbackCacheObserved = urls => command('observed', { urls });
  api.playbackCacheProbe = (url, sessionId) => command('probe', { url, sessionId });
  api.playbackCacheCursor = url => command('cursor', { url });
  api.playbackCachePending = url => command('pending', { url });
  api.clearCache = async () => { await command('clear'); window.dispatchEvent(new Event('playback-cache-change')); };
}
api.boot = async () => ({ user: null, logged_in: false, base: "https://fixture.invalid", strict_security: true });
api.play = async (id, ep = 1) => {
  fixture.playRequests.push({ id, ep });
  if (fixture.failEpisode === ep) throw new Error("Fixture episode unavailable");
  return { id, ep_num: ep, title: "Seek fixture", duration: 90,
    video_url: mediaSource() + (resumeTest ? `?video=${id}&episode=${ep}${mixedDuration && ep === 2 ? '&short=1' : ''}` : "") };
};
api.video = async (id) => ({
  id, title: resumeTest ? `续播视频 ${id}` : "Seek fixture", description: "", cover: "", tags: [], score: 0,
  episode_count: resumeTest ? 8 : 1, total_episodes: resumeTest ? 8 : 1,
  episodes: Array.from({ length: resumeTest ? 8 : 1 }, (_, i) => ({ ep_num: i + 1, title: `episode ${i + 1}`, duration: 90, status: 1 })),
  counts: { like: 0, collect: 0, comment: 0, hot: 0 },
  author: { id: 1, name: "Fixture" }, is_finished: true, is_original: 0,
});
if (resumeTest) {
  const items = [991, 992].map(id => ({
    id, title: `续播视频 ${id}`, cover: "", tags: [], score: 0, hot: 0,
    episode_count: 8, total_episodes: 8, like_count: 0, comment_count: 0, collect_count: 0, is_finished: true, is_original: 0,
  }));
  const pagination = { page: 1, pages: 1, size: 20, total: items.length };
  api.category = async () => ({ items, pagination });
  api.search = async keyword => ({ items, pagination, keyword });
  api.posts = async () => ({ items: [], pagination: { ...pagination, total: 0 } });
}

function ResumeRoutes({ active }: { active: boolean }) {
  const store = useStore();
  const [entry, setEntry] = useState("category");
  fixture.getProgress = store.getWatchProgress;
  fixture.record = store.addHistory;
  fixture.claim = store.claimWatchProgress;
  fixture.clearHistory = store.clearHistory;
  return <>
    <div style={{ position: "relative", width: 900, height: 540 }}>
      {entry === "category" ? <Category wide active={!store.stack.length} />
        : entry === "search" ? <Search wide active={!store.stack.length} /> : <Profile key={entry} wide />}
      {store.stack.map((page, i) => <div key={page.key} className="absolute inset-0 bg-bg" style={{ zIndex: 30 + i }}>
        {page.kind === "feed"
          ? <Feed videoIds={page.videoIds} startIndex={page.index} startEp={page.startEp} startTime={page.startTime}
              mode="context" title={page.title} active={active && i === store.stack.length - 1} bottomInset={0} onBack={store.pop} />
          : page.kind === "channel" ? <ChannelPage cate={page.cate} name={page.name} onBack={store.pop} wide /> : null}
      </div>)}
    </div>
    <button onClick={() => setEntry("category")}>分类入口</button>
    <button onClick={() => setEntry("favorites")}>收藏入口</button>
    <button onClick={() => setEntry("search")}>搜索入口</button>
    <button onClick={store.clearHistory}>清除续播测试记录</button>
  </>;
}
function Fixture() {
  const [active, setActive] = useState(!initiallyInactive);
  const [settings, setSettings] = useState(false);
  const [sourceKey, setSourceKey] = useState(0);
  fixture.sourceKey = sourceKey;
  fixture.nextSource = () => setSourceKey(key => key + 1);
  fixture.setActive = setActive;
  return <AppProvider>
    {resumeTest ? <ResumeRoutes active={active} /> : <div style={{ position: "relative", width: 900, height: 540 }}>
      {legacyTest
        ? <Player key={sourceKey} src={mediaSource()} autoPlay active={active} className="w-full h-full" diagnosticContext={{ video_id: 991, active }} />
        : <Feed key={sourceKey} videoIds={[991]} mode="home" active={active} bottomInset={0} />}
    </div>}
    <button onClick={() => setActive((v) => !v)}>切换播放页激活状态</button>
    {diagnosticTest && <button onClick={() => setSettings(v => !v)}>诊断设置</button>}
    {settings && <div style={{ position: "relative", width: 900, height: 700 }}><Settings onBack={() => setSettings(false)} wide /></div>}
  </AppProvider>;
}
createRoot(document.querySelector("#root")!).render(resumeTest ? <React.StrictMode><Fixture /></React.StrictMode> : <Fixture />);

// 自动检查真正的浏览器命中测试，而非仅匹配 CSS 字符串。
async function checkIdleSeekTarget() {
  const deadline = performance.now() + 20000;
  while (performance.now() < deadline) {
    const video = document.querySelector("video");
    const input = document.querySelector<HTMLInputElement>('input[aria-label="播放进度"]');
    if (video && input && video.currentTime > 0 && document.querySelector(".player-root.is-idle")) {
      const rect = input.getBoundingClientRect();
      if (document.elementFromPoint(rect.x + rect.width * .8, rect.y + rect.height / 2) !== input) {
        throw new Error("闲置进度条点击穿透到视频");
      }
      document.querySelector("#results")!.textContent = JSON.stringify({ passed: 1, failed: 0 });
      return;
    }
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
  }
  throw new Error("等待播放或控制条隐藏超时");
}
if (!legacyTest && !initiallyInactive && !resumeTest) void checkIdleSeekTarget().catch((error) => {
  fixture.errors.push(String(error));
  document.querySelector("#results")!.textContent = JSON.stringify({ passed: 0, failed: 1, error: String(error) });
});

import Hls, { Events, M3U8Parser, type LevelDetails, type LevelLoadedData, type AudioTrackLoadedData, type AudioTrackSwitchingData, type SubtitleTrackLoadedData, type SubtitleTrackSwitchData } from "hls.js";
import { api } from "./api";

export type CacheRange = { start: number; end: number };
type Resource = { url: string; range?: { start: number; end: number }; validator?: string };
type Segment = CacheRange & Resource & { dependencies?: Resource[] };
export const cacheKey = (resource: Resource) => (resource.range ? `${resource.url}\nrange=${resource.range.start}-${resource.range.end}` : resource.url) + (resource.validator ? `\nvalidator=${resource.validator}` : "");
const resources = (segment: Segment) => [...(segment.dependencies ?? []), segment];
const MAX_BLOCK = 16 * 1024 * 1024;
function proxyResource(proxy: string, bytes: number[] = []): Resource | null {
  try {
    const url = new URL(proxy).searchParams.get("u");
    const range = bytes.length ? { start: bytes[0], end: bytes[1] - 1 } : undefined;
    if (!url || (range && (!Number.isSafeInteger(range.start) || !Number.isSafeInteger(range.end) || range.start < 0 || range.start > range.end || range.end - range.start + 1 > MAX_BLOCK))) return null;
    return { url, range };
  } catch { return null; }
}
export function fromDetails(details: LevelDetails): Segment[] {
  if (details.live) return [];
  return details.fragments.flatMap((fragment) => {
    const resource = proxyResource(fragment.url, fragment.byteRange);
    if (!resource || !Number.isFinite(fragment.start) || !Number.isFinite(fragment.duration)) return [];
    const dependencies: Resource[] = [];
    if (fragment.initSegment) {
      const init = proxyResource(fragment.initSegment.url, fragment.initSegment.byteRange);
      if (!init) return [];
      dependencies.push(init);
    }
    const key = fragment.decryptdata;
    if (key?.uri) {
      const dependency = proxyResource(key.uri);
      if (!dependency) return [];
      dependencies.push(dependency);
    }
    return [{ ...resource, dependencies, start: fragment.start, end: fragment.start + fragment.duration }];
  });
}
export function mergeCacheRanges(ranges: CacheRange[]): CacheRange[] {
  const result: CacheRange[] = [];
  for (const range of ranges.filter(r => Number.isFinite(r.start) && Number.isFinite(r.end) && r.end > Math.max(0, r.start)).sort((a, b) => a.start - b.start)) {
    const last = result[result.length - 1];
    if (last && range.start <= last.end + .001) last.end = Math.max(last.end, range.end);
    else result.push({ start: Math.max(0, range.start), end: range.end });
  }
  return result;
}
function bufferRanges(video: HTMLVideoElement): CacheRange[] {
  const result: CacheRange[] = [];
  for (let i = 0; i < video.buffered.length; i++) result.push({ start: video.buffered.start(i), end: video.buffered.end(i) });
  return result;
}
const bufferingEvents = ["playing", "waiting", "timeupdate", "pause", "ratechange"];
function canPrefetch(video: HTMLVideoElement, reserve = 5) {
  const end = bufferRanges(video).find(range => range.start <= video.currentTime && range.end > video.currentTime)?.end ?? video.currentTime;
  return !video.seeking && (video.paused || end - video.currentTime >= reserve * video.playbackRate);
}
type CacheSession = { id: string; ready: Promise<void>; buffered?: boolean };
function startCacheSession(): CacheSession {
  const id = `${Date.now()}-${Math.random().toString(36).slice(2)}-${Math.random().toString(36).slice(2)}`;
  return { id, ready: api.playbackCacheStart(id) };
}
function syncSession(session: CacheSession | undefined, video: HTMLVideoElement) {
  const buffered = canPrefetch(video);
  if (!session || session.buffered === buffered) return;
  session.buffered = buffered;
  session.ready = session.ready.then(() => api.playbackCacheReady(session.id, buffered));
  void session.ready.catch(() => { session.buffered = undefined; });
}
function intersection(a: CacheRange[], b: CacheRange[]) {
  return a.flatMap(left => b.flatMap(right => {
    const start = Math.max(left.start, right.start), end = Math.min(left.end, right.end);
    return end > start ? [{ start, end }] : [];
  }));
}

type Rendition = { id: number; groupId?: string; url?: string };
const sameRendition = (selected: Rendition | null, loaded: { id: number; groupId?: string; track?: { url?: string } }) => !selected || (
  selected.id === loaded.id && (!selected.groupId || selected.groupId === loaded.groupId)
  && (!selected.url || !loaded.track?.url || selected.url === loaded.track.url)
);

/** One serial VOD queue for video and selected audio/subtitles, including their dependencies. */
export function attachPlaybackCache(hls: Hls | null, video: HTMLVideoElement, isActive: () => boolean, report: (ranges: CacheRange[]) => void, nativePlaylist?: string, subtitlesEnabled: () => boolean = () => true) {
  let stopped = false, version = 0, running = false, revision = -1, enabled = false;
  let blocked: { key: string; bytes: number; protectedKeys: string[] } | undefined;
  let timer: number | undefined;
  let diskRanges: CacheRange[] = [];
  const tracks: Record<string, Segment[]> = { main: [], audio: [], subtitle: [] };
  const plans = { audio: new Map<string, Segment[]>(), subtitle: new Map<string, Segment[]>() };
  let audioSelection: Rendition | null = null, subtitleSelection: Rendition | null = null;
  let audioRequired = false, subtitleRequired = false, nativeSubtitleExpected = false;
  const nativeSelected: Record<string, string | null> = { main: null, audio: null, subtitle: null };
  const failures = new Map<string, { count: number; retryAt: number }>();
  let restartNative: (() => void) | undefined;
  let session: CacheSession | undefined;
  const stopSession = () => {
    const old = session; session = undefined;
    if (old) void old.ready.catch(() => {}).then(() => api.playbackCacheStop(old.id)).catch(() => {});
  };
  const emit = () => { if (!stopped) report(enabled ? mergeCacheRanges([...diskRanges, ...bufferRanges(video)]) : []); };
  const wake = (delay = 250) => { if (timer !== undefined) clearTimeout(timer); if (!stopped) timer = window.setTimeout(() => { void run(); }, delay); };
  const reset = (seeking = false) => {
    version++;
    stopSession();
    blocked = undefined;
    failures.clear();
    if (!seeking) diskRanges = [];
    emit();
    wake(seeking ? 0 : 250);
    restartNative?.();
  };
  const activity = () => { if (!isActive()) { version++; stopSession(); } else reset(); };
  const getSession = () => {
    session ??= startCacheSession();
    syncSession(session, video);
    return session;
  };
  const run = async () => {
    if (running || stopped) return;
    if (!isActive()) { activity(); wake(1500); return; }
    running = true;
    let current = version, delay = 1500;
    try {
      const status = await api.playbackCacheStatus();
      if (stopped || current !== version || !isActive()) return;
      if (revision !== status.revision) { revision = status.revision; enabled = !!status.max_mib; reset(); current = version; }
      if (!status.max_mib) { enabled = false; stopSession(); diskRanges = []; emit(); return; }
      enabled = true;
      const media = [...tracks.main, ...tracks.audio];
      const subtitles = subtitlesEnabled() ? tracks.subtitle : [];
      const snapshot = [...media, ...subtitles];
      if (!snapshot.length) { emit(); return; }
      const unique = new Map(snapshot.flatMap(resources).map(resource => [cacheKey(resource), resource]));
      const keys = [...unique.keys()];
      const present = await api.playbackCacheContains(keys);
      if (stopped || current !== version || !isActive()) return;
      const available = new Set(keys.filter((_, i) => present[i]));
      const complete = (segment: Segment) => resources(segment).every(resource => available.has(cacheKey(resource)));
      const main = mergeCacheRanges(tracks.main.filter(complete));
      diskRanges = audioRequired ? intersection(main, mergeCacheRanges(tracks.audio.filter(complete))) : main;
      if (subtitlesEnabled() && subtitleRequired) diskRanges = intersection(diskRanges, mergeCacheRanges(tracks.subtitle.filter(complete)));
      emit();
      syncSession(session, video);
      if (!canPrefetch(video)) return;
      const time = video.currentTime;
      const bufferedEnd = bufferRanges(video).find(range => range.start <= time && range.end > time)?.end ?? time;
      const upcoming = [...media.filter(segment => segment.end > bufferedEnd), ...subtitles.filter(segment => segment.end > time)]
        .sort((a, b) => a.start - b.start);
      const candidates = new Map<string, { resource: Resource; start: number }>();
      for (const segment of upcoming) {
        for (const resource of resources(segment)) {
          const key = cacheKey(resource);
          if (!candidates.has(key)) candidates.set(key, { resource, start: segment.start });
        }
      }
      const next = [...candidates].find(([key]) => !available.has(key) && (failures.get(key)?.count ?? 0) < 3 && (failures.get(key)?.retryAt ?? 0) <= Date.now());
      if (!next) return;
      const [key, { resource, start }] = next;
      // Fill the first gap before protecting cached fragments farther ahead.
      const protectedKeys = new Set([...candidates].filter(([, candidate]) => candidate.start <= start).map(([key]) => key));
      if (blocked?.key === key && status.bytes >= blocked.bytes
        && blocked.protectedKeys.every(key => protectedKeys.has(key) && available.has(key))) return;
      const currentSession = getSession();
      await currentSession.ready;
      if (stopped || current !== version || !isActive() || session !== currentSession) return;
      try {
        const saved = await api.playbackCachePrefetch(resource.url, [...protectedKeys], currentSession.id, resource.range);
        if (stopped || current !== version || !isActive()) return;
        if (saved) { blocked = undefined; failures.delete(key); }
        else blocked = { key, bytes: status.bytes, protectedKeys: [...protectedKeys].filter(key => available.has(key)) };
        delay = 0; // Recheck protection in case playback buffered more while the download was in flight.
      } catch {
        if (current !== version || stopped) return;
        const count = (failures.get(key)?.count ?? 0) + 1;
        failures.set(key, { count, retryAt: Date.now() + 500 * 2 ** count });
        delay = 0; // Other fragments can still be downloaded while this resource backs off.
      }
    } catch { stopSession(); delay = 3000; }
    finally { running = false; wake(current !== version ? 0 : delay); }
  };
  const setTrack = (name: string, details: LevelDetails) => { tracks[name] = fromDetails(details); reset(); };
  const levelLoaded = (_event: Events.LEVEL_LOADED, data: LevelLoadedData) => setTrack("main", data.details);
  const audioSwitching = (_event: Events.AUDIO_TRACK_SWITCHING, data: AudioTrackSwitchingData) => {
    audioSelection = data;
    audioRequired = !!data.url;
    const details = data.details ?? hls?.audioTracks?.[data.id]?.details;
    tracks.audio = data.url ? (details ? fromDetails(details) : plans.audio.get(data.url) ?? []) : [];
    reset();
  };
  const audioLoaded = (_event: Events.AUDIO_TRACK_LOADED, data: AudioTrackLoadedData) => {
    if (!sameRendition(audioSelection, data) || (typeof hls?.audioTrack === "number" && hls.audioTrack !== data.id)) return;
    const url = data.track?.url ?? audioSelection?.url;
    if (url) plans.audio.set(url, fromDetails(data.details));
    audioRequired = true; setTrack("audio", data.details);
  };
  const subtitleSwitching = (_event: Events.SUBTITLE_TRACK_SWITCH, data: SubtitleTrackSwitchData) => {
    subtitleSelection = data.id >= 0 ? data : null;
    subtitleRequired = subtitlesEnabled() && data.id >= 0 && !!data.url;
    const details = data.id >= 0 ? hls?.subtitleTracks?.[data.id]?.details : undefined;
    tracks.subtitle = subtitleRequired && data.url ? (details ? fromDetails(details) : plans.subtitle.get(data.url) ?? []) : [];
    reset();
  };
  const subtitleLoaded = (_event: Events.SUBTITLE_TRACK_LOADED, data: SubtitleTrackLoadedData) => {
    if (!sameRendition(subtitleSelection, data) || (typeof hls?.subtitleTrack === "number" && hls.subtitleTrack !== data.id)) return;
    const url = data.track?.url ?? subtitleSelection?.url;
    if (url) plans.subtitle.set(url, fromDetails(data.details));
    if (!subtitlesEnabled()) return;
    subtitleRequired = true; setTrack("subtitle", data.details);
  };
  const subtitlePreference = () => {
    if (!subtitlesEnabled()) { tracks.subtitle = []; subtitleRequired = false; nativeSelected.subtitle = null; reset(); return; }
    if (hls) {
      const id = hls.subtitleTrack;
      const track = hls.subtitleTracks?.[id];
      subtitleSwitching(Hls.Events.SUBTITLE_TRACK_SWITCH, { id, url: track?.url, groupId: track?.groupId });
    } else { tracks.subtitle = []; subtitleRequired = nativeSubtitleExpected; nativeSelected.subtitle = null; reset(); }
  };
  hls?.on(Hls.Events.LEVEL_LOADED, levelLoaded);
  hls?.on(Hls.Events.AUDIO_TRACK_SWITCHING, audioSwitching);
  hls?.on(Hls.Events.AUDIO_TRACK_LOADED, audioLoaded);
  hls?.on(Hls.Events.SUBTITLE_TRACK_SWITCH, subtitleSwitching);
  hls?.on(Hls.Events.SUBTITLE_TRACK_LOADED, subtitleLoaded);
  const abort = new AbortController();
  let nativeRunning = false, nativeReady = false;
  const loadNative = async () => {
    if (!nativePlaylist || nativeRunning || nativeReady || stopped) return;
    nativeRunning = true;
    const wait = () => new Promise<void>(resolve => window.setTimeout(resolve, 2000));
    const manifest = async (url: string): Promise<string | null> => {
      const key = `manifest:${url}`;
      while (!stopped && (failures.get(key)?.count ?? 0) < 3) {
        if (!isActive()) { await wait(); continue; }
        try {
          const response = await fetch(url, { signal: abort.signal });
          if (!response.ok) throw new Error(`HTTP ${response.status}`);
          const text = await response.text(); failures.delete(key); return text;
        } catch {
          if (stopped) return null;
          failures.set(key, { count: (failures.get(key)?.count ?? 0) + 1, retryAt: 0 });
          await wait();
        }
      }
      return null;
    };
    try {
      while (!stopped && (!(await api.playbackCacheStatus()).max_mib || !isActive())) await wait();
      if (stopped) return;
      const text = await manifest(nativePlaylist);
      if (!text || stopped) return;
      const parse = (text: string, url: string, type: "main" | "audio" | "subtitle") => M3U8Parser.parseLevelPlaylist(text, url, 0, type as Parameters<typeof M3U8Parser.parseLevelPlaylist>[3], 0, null);
      if (M3U8Parser.isMediaPlaylist(text)) { if (!stopped) { setTrack("main", parse(text, nativePlaylist, "main")); nativeReady = true; } return; }
      const master = M3U8Parser.parseMasterPlaylist(text, nativePlaylist);
      const media = M3U8Parser.parseMasterPlaylistMedia(text, nativePlaylist, master);
      const lists = { main: master.levels, audio: media.AUDIO ?? [], subtitle: media.SUBTITLES ?? [] };
      const selected = nativeSelected;
      // Native track selection is observed from protocol requests, never guessed by language or bitrate.
      while (!stopped) {
        if (!isActive()) { await wait(); continue; }
        if (!(await api.playbackCacheStatus()).max_mib) { reset(); selected.main = selected.audio = selected.subtitle = null; tracks.main = []; tracks.audio = []; tracks.subtitle = []; audioRequired = subtitleRequired = false; await wait(); continue; }
        for (const type of ["main", "audio", "subtitle"] as const) {
          if (type === "subtitle" && !subtitlesEnabled()) {
            if (selected.subtitle !== null || tracks.subtitle.length) { tracks.subtitle = []; subtitleRequired = false; selected.subtitle = null; reset(); }
            continue;
          }
          const main = lists.main.find(level => level.url === selected.main);
          const group = type === "main" ? undefined : main?.attrs[type === "audio" ? "AUDIO" : "SUBTITLES"];
          const list = lists[type].filter(level => proxyResource(level.url) && (type === "main" || (!!group && "groupId" in level && level.groupId === group)));
          if (!list.length) {
            if (type !== "main" && (selected[type] !== null || tracks[type].length || (type === "audio" ? audioRequired : subtitleRequired))) {
              tracks[type] = []; selected[type] = null;
              if (type === "audio") audioRequired = false; else subtitleRequired = false;
              reset();
            }
            continue;
          }
          const index = type === "main" && list.length === 1 ? 0 : await api.playbackCacheObserved(list.map(level => proxyResource(level.url)!.url));
          if (index === null || index < 0 || index >= list.length || selected[type] === list[index].url) continue;
          // Invalidate the previous selection before awaiting the new playlist.
          if (type === "main") {
            const next = lists.main.find(level => level.url === list[index].url);
            nativeSubtitleExpected = !!next?.attrs.SUBTITLES && lists.subtitle.some(track => track.groupId === next.attrs.SUBTITLES && !!proxyResource(track.url));
            for (const auxiliary of ["audio", "subtitle"] as const) {
              const attr = auxiliary === "audio" ? "AUDIO" : "SUBTITLES";
              if (main?.attrs[attr] !== next?.attrs[attr]) {
                tracks[auxiliary] = []; selected[auxiliary] = null;
                if (auxiliary === "audio") audioRequired = !!next?.attrs[attr];
                else subtitleRequired = subtitlesEnabled() && !!next?.attrs[attr];
              }
            }
          }
          selected[type] = null; tracks[type] = [];
          if (type === "audio") audioRequired = true;
          if (type === "subtitle") subtitleRequired = true;
          reset();
          const currentVersion = version;
          const text = await manifest(list[index].url);
          if (!text || stopped || currentVersion !== version || (type === "subtitle" && !subtitlesEnabled())) continue;
          const details = parse(text, list[index].url, type);
          setTrack(type, details); selected[type] = list[index].url;
        }
        await wait();
      }
    } catch { /* The system player keeps its ordinary network path. */ }
    finally { nativeRunning = false; }
  };
  if (!hls && nativePlaylist) restartNative = () => { void loadNative(); };
  const onCacheChange = () => { enabled = false; reset(); };
  const seeking = () => reset(true);
  const buffering = () => { syncSession(session, video); if (!running && canPrefetch(video)) wake(0); };
  const progress = () => { emit(); buffering(); };
  hls?.on(Hls.Events.FRAG_BUFFERED, progress);
  video.addEventListener("seeking", seeking);
  video.addEventListener("subtitle-preference", subtitlePreference);
  video.addEventListener("progress", progress);
  video.addEventListener("playback-activity", activity);
  for (const event of bufferingEvents) video.addEventListener(event, buffering);
  window.addEventListener("playback-cache-change", onCacheChange);
  wake();
  return () => {
    stopped = true; version++; stopSession(); if (timer !== undefined) clearTimeout(timer); abort.abort();
    hls?.off(Hls.Events.LEVEL_LOADED, levelLoaded); hls?.off(Hls.Events.AUDIO_TRACK_SWITCHING, audioSwitching); hls?.off(Hls.Events.AUDIO_TRACK_LOADED, audioLoaded);
    hls?.off(Hls.Events.SUBTITLE_TRACK_SWITCH, subtitleSwitching); hls?.off(Hls.Events.SUBTITLE_TRACK_LOADED, subtitleLoaded);
    hls?.off(Hls.Events.FRAG_BUFFERED, progress);
    video.removeEventListener("seeking", seeking); video.removeEventListener("subtitle-preference", subtitlePreference); video.removeEventListener("progress", progress); video.removeEventListener("playback-activity", activity);
    for (const event of bufferingEvents) video.removeEventListener(event, buffering);
    window.removeEventListener("playback-cache-change", onCacheChange); report([]);
  };
}

/** Direct files use byte blocks; byte offsets are not treated as media timestamps. */
export function attachDirectCache(proxyUrl: string, video: HTMLVideoElement, isActive: () => boolean, report: (ranges: CacheRange[]) => void) {
  let url = "";
  try { url = new URL(proxyUrl).searchParams.get("u") || ""; } catch { /* invalid source */ }
  let stopped = false, running = false, total = 0, offset = 0, version = 0, validator = "", blockedCursor = -1, lastLimit = -1, revision = -1, lastCursor = -1, failures = 0, enabled = false;
  let timer: number | undefined;
  let session: CacheSession | undefined;
  let filling = false;
  const fillFailures = new Map<number, { count: number; retryAt: number }>();
  const block = 1024 * 1024;
  const stopSession = () => {
    const old = session; session = undefined;
    if (old) void old.ready.catch(() => {}).then(() => api.playbackCacheStop(old.id)).catch(() => {});
  };
  const emit = () => { if (!stopped) report(enabled ? mergeCacheRanges(bufferRanges(video)) : []); };
  const wake = (delay = 250) => { if (timer !== undefined) clearTimeout(timer); timer = window.setTimeout(() => { void run(); }, delay); };
  const run = async () => {
    if (running || stopped) return;
    if (!url || !isActive()) { stopSession(); wake(1500); return; }
    emit();
    running = true; let current = version; let delay = 1500;
    try {
      const status = await api.playbackCacheStatus();
      if (stopped || current !== version || !isActive()) return;
      if (revision !== status.revision) { revision = status.revision; enabled = !!status.max_mib; reset(); current = version; }
      const { max_mib } = status;
      if (!max_mib) { enabled = false; stopSession(); emit(); return; }
      enabled = true;
      syncSession(session, video);
      if (!canPrefetch(video)) return;
      if (lastLimit !== max_mib) { lastLimit = max_mib; blockedCursor = -1; }
      if (failures >= 3) return;
      if (!total) {
        const currentSession = getSession();
        await currentSession.ready;
        if (stopped || current !== version || !isActive()) return;
        const info = await api.playbackCacheProbe(url, currentSession.id);
        if (stopped || current !== version || !isActive()) return;
        total = info.total; validator = info.tag;
        if (!Number.isSafeInteger(total) || total <= 0) { total = 0; return; }
      }
      const cursor = await api.playbackCacheCursor(url);
      if (stopped || current !== version || !isActive()) return;
      void fill(current, cursor);
      const start = Math.floor(cursor / block) * block;
      if (start !== lastCursor) { lastCursor = start; offset = start; failures = 0; blockedCursor = -1; }
      if (start === blockedCursor || failures >= 3 || cursor >= total || offset >= total) return;
      if (offset < start) offset = start;
      const list: Segment[] = [];
      for (let i = offset; i < total && list.length < 64; i += block) {
        list.push({ url, validator, start: 0, end: 0, range: { start: i, end: Math.min(i + block, total) - 1 } });
      }
      if (!list.length) return;
      const present = await api.playbackCacheContains(list.map(cacheKey));
      if (stopped || current !== version || !isActive()) return;
      const index = present.findIndex((value) => !value);
      if (index < 0) { offset = list[list.length - 1].range!.end + 1; if (offset < total) delay = 0; return; }
      offset = list[index].range!.start;
      const currentSession = getSession();
      await currentSession.ready;
      if (stopped || current !== version || !isActive() || session !== currentSession) return;
      const protectedKeys: string[] = [];
      for (let i = start; i < offset && protectedKeys.length < 4096; i += block) {
        protectedKeys.push(cacheKey({ url, validator, range: { start: i, end: Math.min(i + block, total) - 1 } }));
      }
      const saved = await api.playbackCachePrefetch(url, protectedKeys, currentSession.id, list[index].range, validator, cursor);
      if (stopped || current !== version || !isActive()) return;
      if (saved) { offset += block; failures = 0; delay = 0; }
      else blockedCursor = start;
    } catch {
      if (stopped || current !== version) return;
      failures++; stopSession(); total = 0; validator = ""; offset = 0; delay = 2000 * 2 ** failures;
    }
    finally { running = false; if (!stopped) wake(current !== version ? 0 : delay); }
  };
  const getSession = () => {
    session ??= startCacheSession();
    syncSession(session, video);
    return session;
  };
  const fill = async (current: number, cursor: number) => {
    if (filling || video.readyState < 3 || !canPrefetch(video, 10)) return;
    filling = true;
    let again = false;
    let target: { start: number; end: number } | undefined;
    try {
      const pending = await api.playbackCachePending(url);
      if (stopped || current !== version || !isActive()) return;
      target = pending.sort((a, b) => Math.abs(a.start - cursor) - Math.abs(b.start - cursor)).find(range => {
        const failure = fillFailures.get(range.start);
        return !failure || (failure.count < 3 && failure.retryAt <= Date.now());
      });
      if (!target) return;
      const protectedKeys = [cacheKey({ url, validator, range: target })];
      for (let i = Math.floor(cursor / block) * block; i < offset && protectedKeys.length < 4096; i += block) {
        protectedKeys.push(cacheKey({ url, validator, range: { start: i, end: Math.min(i + block, total) - 1 } }));
      }
      const currentSession = getSession();
      await currentSession.ready;
      if (stopped || current !== version || !isActive() || session !== currentSession) return;
      const saved = await api.playbackCachePrefetch(url, protectedKeys, currentSession.id, target, validator);
      if (stopped || current !== version || !isActive()) return;
      if (!saved) throw new Error("文件块尚未写入");
      fillFailures.delete(target.start);
      again = true;
    } catch {
      if (target && current === version && !stopped) {
        const count = (fillFailures.get(target.start)?.count ?? 0) + 1;
        fillFailures.set(target.start, { count, retryAt: Date.now() + 1000 * 2 ** count });
      }
    } finally { filling = false; if (!stopped && (again || current !== version)) wake(0); }
  };
  const reset = () => { version++; offset = 0; total = 0; validator = ""; failures = 0; fillFailures.clear(); blockedCursor = -1; lastCursor = -1; stopSession(); wake(); };
  const seeking = () => { version++; offset = 0; failures = 0; fillFailures.clear(); blockedCursor = -1; lastCursor = -1; stopSession(); wake(750); };
  const seeked = () => { if (!running && isActive()) wake(0); };
  const buffering = () => { syncSession(session, video); if (!running && canPrefetch(video)) wake(0); };
  const progress = () => { emit(); buffering(); };
  const activity = () => { if (!isActive()) { version++; stopSession(); } else reset(); };
  video.addEventListener("progress", progress);
  video.addEventListener("seeking", seeking);
  video.addEventListener("seeked", seeked);
  video.addEventListener("playback-activity", activity);
  for (const event of bufferingEvents) video.addEventListener(event, buffering);
  const onCacheChange = () => { enabled = false; emit(); reset(); };
  window.addEventListener("playback-cache-change", onCacheChange);
  wake();
  return () => { stopped = true; version++; stopSession(); if (timer !== undefined) clearTimeout(timer); video.removeEventListener("progress", progress); video.removeEventListener("seeking", seeking); video.removeEventListener("seeked", seeked); video.removeEventListener("playback-activity", activity); for (const event of bufferingEvents) video.removeEventListener(event, buffering); window.removeEventListener("playback-cache-change", onCacheChange); report([]); };
}

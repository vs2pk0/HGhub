// Deterministic queue tests: real manifest parsing and production scheduler, mocked IPC and time.
import assert from 'node:assert/strict';
import { build } from 'esbuild';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import Hls, { M3U8Parser } from 'hls.js';

const temporary = await mkdtemp(join(tmpdir(), 'hghub-cache-tests-'));
const realNow = Date.now;
try {
  const bundle = join(temporary, 'cache.mjs');
  await build({ entryPoints: [resolve('src/playbackCache.ts')], outfile: bundle, bundle: true, format: 'esm', platform: 'node', plugins: [{ name: 'ipc-fixture', setup(builder) {
    builder.onResolve({ filter: /^\.\/api$/ }, () => ({ path: 'api', namespace: 'fixture' }));
    builder.onLoad({ filter: /.*/, namespace: 'fixture' }, () => ({ contents: 'export const api = globalThis.cacheFixtureApi;', loader: 'js' }));
  }}] });
  let now = 0, nextTimer = 0;
  const timers = new Map();
  const window = new EventTarget();
  window.setTimeout = (fn, delay = 0) => { const id = ++nextTimer; timers.set(id, { fn, at: now + delay }); return id; };
  window.clearTimeout = id => timers.delete(id);
  globalThis.window = window;
  // The scheduler uses clearTimeout for handles created through window.
  globalThis.clearTimeout = window.clearTimeout;
  Date.now = () => now;
  const flush = async () => { for (let i = 0; i < 40; i++) await Promise.resolve(); };
  const tick = async ms => {
    const target = now + ms;
    await flush();
    for (let iterations = 0; iterations < 500; iterations++) {
      const pending = [...timers].filter(([, task]) => task.at <= target).sort((a, b) => a[1].at - b[1].at)[0];
      if (!pending) { now = target; await flush(); return; }
      timers.delete(pending[0]); now = pending[1].at; pending[1].fn(); await flush();
    }
    throw new Error('Scheduler spun without yielding');
  };
  let config, cached, capacity, downloads, pending, active, mode, queries, starts, stops;
  function reset() {
    timers.clear(); now = 0;
    config = { max_mib: 128, bytes: 0, revision: 0 }; cached = new Set(); capacity = Infinity; downloads = []; pending = new Map();
    active = true; mode = 'success'; queries = 0; starts = []; stops = [];
  }
  const key = (url, range, validator) => url + (range ? `\nrange=${range.start}-${range.end}` : '') + (validator ? `\nvalidator=${validator}` : '');
  globalThis.cacheFixtureApi = {
    playbackCacheStatus: async () => ({ ...config }),
    playbackCacheContains: async keys => { queries++; return keys.map(k => cached.has(k)); },
    playbackCacheStart: async id => { starts.push(id); },
    playbackCacheStop: async id => {
      stops.push(id);
      const waits = pending.get(id);
      for (const reject of Array.isArray(waits) ? waits : waits ? [waits] : []) reject(new Error('cancelled'));
      pending.delete(id);
    },
    playbackCachePrefetch: async (url, protectedKeys, id, range, validator, focus) => {
      downloads.push({ url, id, range, validator, focus });
      if (mode === 'hold') return new Promise((_, reject) => pending.set(id, [...(pending.get(id) ?? []), reject]));
      if (mode === 'fail-first' && url.endsWith('/seg0')) throw new Error('HTTP 503');
      if (mode === 'full') return false;
      if (cached.size >= capacity) {
        const victim = [...cached].find(key => !protectedKeys.includes(key));
        if (!victim) return false;
        cached.delete(victim);
      }
      cached.add(key(url, range, validator)); return true;
    },
    playbackCacheProbe: async () => ({ total: 2 * 1024 * 1024, tag: 'etag' }),
    playbackCacheCursor: async () => 0,
    playbackCacheObserved: async () => null,
    playbackCacheReady: async () => {},
    playbackCachePending: async () => [],
  };
  const { attachPlaybackCache, attachDirectCache, fromDetails, mergeCacheRanges, cacheKey } = await import(pathToFileURL(bundle));
  class Video extends EventTarget { currentTime = 0; readyState = 4; paused = true; seeking = false; playbackRate = 1; buffered = { length: 0, start() {}, end() {} }; }
  class Engine {
    callbacks = new Map();
    on(event, handler) { this.callbacks.set(event, handler); }
    off(event) { this.callbacks.delete(event); }
    details(details) { this.callbacks.get(Hls.Events.LEVEL_LOADED)?.(Hls.Events.LEVEL_LOADED, { details }); }
  }
  const proxy = path => `http://hg.localhost/seg?u=${encodeURIComponent(`https://media.test/${path}`)}`;
  const parse = text => M3U8Parser.parseLevelPlaylist(text, 'http://hg.localhost/hls?u=x', 0, 'main', 0, null);
  const plain = parse(`#EXTM3U\n#EXT-X-TARGETDURATION:4\n${[0, 1, 2].map(n => `#EXTINF:4,\n${proxy(`seg${n}`)}`).join('\n')}\n#EXT-X-ENDLIST\n`);
  const encrypted = parse(`#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MAP:URI="${proxy('init')}",BYTERANGE="4@0"\n#EXT-X-KEY:METHOD=AES-128,URI="${proxy('key')}"\n#EXTINF:4,\n${proxy('seg0')}\n#EXT-X-ENDLIST\n`);
  let passed = 0;
  async function check(name, fn) { reset(); await fn(); passed++; console.log(`PASS ${name}`); }
  await check('manifest dependencies and range identity', async () => {
    const plan = fromDetails(encrypted);
    assert.deepEqual(plan[0].dependencies.map(r => r.url), ['https://media.test/init', 'https://media.test/key']);
    assert.match(cacheKey(plan[0].dependencies[0]), /range=0-3$/);
    assert.deepEqual(mergeCacheRanges([{ start: 4, end: 8 }, { start: 0, end: 4 }, { start: 12, end: 16 }]), [{ start: 0, end: 8 }, { start: 12, end: 16 }]);
  });
  await check('dependencies complete before showing disk ranges', async () => {
    const video = new Video(), engine = new Engine(); let ranges = [];
    const dispose = attachPlaybackCache(engine, video, () => active, value => ranges = value);
    engine.details(encrypted); await tick(500);
    assert.deepEqual(downloads.map(d => d.url), ['https://media.test/init', 'https://media.test/key', 'https://media.test/seg0']);
    assert.deepEqual(ranges, [{ start: 0, end: 4 }]); dispose(); await flush();
  });
  await check('broken segment has bounded retries without blocking later segments', async () => {
    mode = 'fail-first'; const video = new Video(), engine = new Engine();
    const dispose = attachPlaybackCache(engine, video, () => active, () => {});
    engine.details(plain); await tick(20000);
    assert.equal(downloads.filter(d => d.url.endsWith('/seg0')).length, 3);
    assert(cached.has('https://media.test/seg1') && cached.has('https://media.test/seg2')); dispose(); await flush();
  });
  await check('seek and inactive page cancel old work', async () => {
    mode = 'hold'; const video = new Video(), engine = new Engine();
    const dispose = attachPlaybackCache(engine, video, () => active, () => {});
    engine.details(plain); await tick(500); const old = downloads[0].id;
    video.currentTime = 8; video.dispatchEvent(new Event('seeking')); await tick(500);
    assert(stops.includes(old)); assert.equal(downloads.at(-1).url, 'https://media.test/seg2');
    const latest = downloads.at(-1).id; active = false; video.dispatchEvent(new Event('playback-activity')); await tick(500);
    assert(stops.includes(latest)); const count = downloads.length; await tick(5000); assert.equal(downloads.length, count);
    dispose(); await flush();
  });
  await check('seeking into a full cache fills the gap before a later cached range', async () => {
    const video = new Video(), engine = new Engine();
    const [first, gap, later] = [0, 1, 2].map(n => `https://media.test/seg${n}`);
    capacity = 2;
    cached.add(first).add(later);
    video.currentTime = 9;
    const dispose = attachPlaybackCache(engine, video, () => active, () => {});
    engine.details(plain); await tick(500);
    video.currentTime = 1; video.dispatchEvent(new Event('seeking')); await tick(500);
    assert(cached.has(first), 'Keep the seek target on disk until it is buffered');
    assert(cached.has(gap), 'Fill the next gap before playback reaches it');

    video.buffered = { length: 1, start: () => 0, end: () => 4 };
    video.dispatchEvent(new Event('progress')); await tick(100);
    assert(cached.has(gap) && cached.has(later), 'Reclaim a buffered block to extend the disk cache');
    const count = downloads.length; await tick(5000);
    assert.equal(downloads.length, count, 'Do not fetch the reclaimed block again while it is buffered');
    dispose(); await flush();
  });
  await check('capacity changes resume a full queue and zero stops it', async () => {
    mode = 'full'; const video = new Video(), engine = new Engine();
    const dispose = attachPlaybackCache(engine, video, () => active, () => {});
    engine.details(plain); await tick(500); assert.equal(downloads.length, 1);
    mode = 'success'; config.revision++; config.max_mib = 256; window.dispatchEvent(new Event('playback-cache-change')); await tick(500);
    assert.equal(downloads.length, 4);
    config.max_mib = 0; config.revision++; cached.clear(); window.dispatchEvent(new Event('playback-cache-change')); await tick(5000);
    assert.equal(downloads.length, 4); dispose(); await flush();
  });
  await check('background transfers follow the playback buffer without changing HLS boundaries', async () => {
    const video = new Video(), engine = new Engine();
    video.paused = false;
    let end = 2;
    video.currentTime = 1.5;
    video.buffered = { length: 1, start: () => 0, end: () => end };
    const states = [];
    const original = globalThis.cacheFixtureApi.playbackCacheReady;
    globalThis.cacheFixtureApi.playbackCacheReady = async (id, ready) => { states.push({ id, ready }); };
    mode = 'hold';
    const dispose = attachPlaybackCache(engine, video, () => active, () => {});
    try {
      engine.details(plain); await tick(1000);
      assert.equal(downloads.length, 0, 'Leave bandwidth to playback when the buffer is short');
      end = 7; video.dispatchEvent(new Event('progress')); await tick(100);
      assert.equal(downloads[0].url, 'https://media.test/seg1', 'Use the original 4-8 second fragment containing the buffer end');
      assert.equal(states.at(-1).ready, true);
      end = 3; video.dispatchEvent(new Event('timeupdate')); await flush();
      assert.equal(states.at(-1).ready, false, 'Pause an in-flight background download as the buffer drains');
      end = 8; video.dispatchEvent(new Event('playing')); await flush();
      assert.equal(states.at(-1).ready, true);
      assert.equal(new Set(states.map(state => state.id)).size, 1, 'Resume the same session');
    } finally { dispose(); await flush(); globalThis.cacheFixtureApi.playbackCacheReady = original; }
  });
  await check('direct file finishes once and does not rescan at EOF', async () => {
    const video = new Video();
    const dispose = attachDirectCache(`${proxy('movie.mp4')}&direct=1`, video, () => active, () => {});
    await tick(500); assert.equal(downloads.length, 2);
    const before = queries; await tick(10000); assert.equal(downloads.length, 2); assert.equal(queries, before);
    dispose(); await flush();
  });
  await check('direct seek fills the first missing block and releases served blocks', async () => {
    const video = new Video(), url = 'https://media.test/movie.mp4';
    const block = 1024 * 1024, total = 4 * block;
    const blockKey = index => key(url, { start: index * block, end: (index + 1) * block - 1 }, 'etag');
    const fixtureApi = globalThis.cacheFixtureApi;
    const { playbackCacheProbe, playbackCacheCursor } = fixtureApi;
    let cursor = total;
    fixtureApi.playbackCacheProbe = async () => ({ total, tag: 'etag' });
    fixtureApi.playbackCacheCursor = async () => cursor;
    capacity = 2;
    cached.add(blockKey(0)).add(blockKey(3));
    const dispose = attachDirectCache(proxy('movie.mp4'), video, () => active, () => {});
    try {
      await tick(500);
      assert.equal(downloads.length, 0, 'Cursor at EOF has no blocks to prefetch');
      video.dispatchEvent(new Event('seeking'));
      cursor = 0;
      video.dispatchEvent(new Event('seeked')); await tick(100);
      assert(cached.has(blockKey(0)) && cached.has(blockKey(1)), 'Prioritize the first gap from the range request');
      cursor = block;
      video.dispatchEvent(new Event('progress')); await tick(100);
      assert(cached.has(blockKey(1)) && cached.has(blockKey(2)), 'Advance beyond bytes already delivered to playback');
    } finally {
      dispose(); await flush();
      Object.assign(fixtureApi, { playbackCacheProbe, playbackCacheCursor });
    }
  });
  await check('direct forward prefetch and partial-block completion run concurrently', async () => {
    const video = new Video(), block = 1024 * 1024;
    video.paused = false;
    video.currentTime = 40;
    video.buffered = { length: 1, start: () => 35, end: () => 60 };
    const fixtureApi = globalThis.cacheFixtureApi;
    const { playbackCacheCursor, playbackCachePending } = fixtureApi;
    fixtureApi.playbackCacheCursor = async () => block + 100;
    fixtureApi.playbackCachePending = async () => [{ start: 0, end: block - 1 }];
    mode = 'hold';
    const dispose = attachDirectCache(proxy('movie.mp4'), video, () => active, () => {});
    try {
      await tick(500);
      assert.deepEqual(downloads.map(item => item.range.start).sort((a, b) => a - b), [0, block]);
      assert(downloads.every(item => item.range.end - item.range.start + 1 === block));
      assert.equal(new Set(downloads.map(item => item.id)).size, 1);
      assert.equal(downloads.find(item => item.range.start === block).focus, block + 100, 'Continue forward from the received position inside the fixed block');
      assert.equal(downloads.find(item => item.range.start === 0).focus, undefined, 'Complete the missing prefix independently');
      const count = downloads.length;
      active = false; video.dispatchEvent(new Event('playback-activity')); await tick(1000);
      assert.equal(downloads.length, count);
      assert(stops.length > 0, 'Cancel both directions when the player becomes inactive');
    } finally { dispose(); await flush(); Object.assign(fixtureApi, { playbackCacheCursor, playbackCachePending }); }
  });
  await check('native VOD uses the same sequential queue', async () => {
    globalThis.fetch = async () => ({ ok: true, text: async () => `#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\n${proxy('native0')}\n#EXTINF:4,\n${proxy('native1')}\n#EXT-X-ENDLIST\n` });
    const video = new Video();
    const dispose = attachPlaybackCache(null, video, () => active, () => {}, 'http://hg.localhost/hls?u=native');
    await tick(1000);
    assert.deepEqual(downloads.map(d => d.url), ['https://media.test/native0', 'https://media.test/native1']);
    dispose(); await flush();
  });
  await check('native manifest can retry after a later seek', async () => {
    let requests = 0, fail = true;
    globalThis.fetch = async () => { requests++; if (fail) throw new Error('temporary outage'); return { ok: true, text: async () => `#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\n${proxy('recovered')}\n#EXT-X-ENDLIST\n` }; };
    const video = new Video(); const dispose = attachPlaybackCache(null, video, () => active, () => {}, 'http://hg.localhost/hls?u=native');
    await tick(10000); assert.equal(requests, 3); assert.equal(downloads.length, 0);
    fail = false; video.dispatchEvent(new Event('seeking')); await tick(1000);
    assert.equal(downloads.at(-1).url, 'https://media.test/recovered'); dispose(); await flush();
  });
  await check('selected audio must be cached before disk progress is complete', async () => {
    const video = new Video(), engine = new Engine(); let ranges = [];
    const dispose = attachPlaybackCache(engine, video, () => active, value => ranges = value);
    engine.details(plain); await tick(500);
    const audio = parse(`#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\n${proxy('audio0')}\n#EXT-X-ENDLIST\n`);
    mode = 'hold'; engine.callbacks.get(Hls.Events.AUDIO_TRACK_LOADED)(Hls.Events.AUDIO_TRACK_LOADED, { details: audio });
    await tick(500); assert.deepEqual(ranges, []); assert.equal(downloads.at(-1).url, 'https://media.test/audio0');
    dispose(); await flush();
  });
  await check('video audio and subtitles share one dependency-aware queue', async () => {
    const video = new Video(), engine = new Engine(); let ranges = [];
    const audio = parse(`#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MAP:URI="${proxy('audio-init')}",BYTERANGE="2@0"\n#EXT-X-KEY:METHOD=AES-128,URI="${proxy('shared-key')}"\n#EXTINF:4,\n${proxy('audio-sync')}\n#EXT-X-ENDLIST\n`);
    const text = parse(`#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MAP:URI="${proxy('text-init')}",BYTERANGE="2@0"\n#EXT-X-KEY:METHOD=AES-128,URI="${proxy('shared-key')}"\n#EXTINF:4,\n${proxy('text-sync.vtt')}\n#EXT-X-ENDLIST\n`);
    const dispose = attachPlaybackCache(engine, video, () => active, value => ranges = value);
    engine.details(plain);
    engine.callbacks.get(Hls.Events.AUDIO_TRACK_LOADED)(Hls.Events.AUDIO_TRACK_LOADED, { id: 0, groupId: 'audio', details: audio });
    engine.callbacks.get(Hls.Events.SUBTITLE_TRACK_LOADED)(Hls.Events.SUBTITLE_TRACK_LOADED, { id: 0, groupId: 'text', details: text });
    await tick(1000);
    for (const path of ['audio-sync', 'text-sync.vtt', 'shared-key']) assert(cached.has(`https://media.test/${path}`));
    assert(cached.has(`${'https://media.test/audio-init'}\nrange=0-1`));
    assert(cached.has(`${'https://media.test/text-init'}\nrange=0-1`));
    assert.equal(downloads.filter(d => d.url.endsWith('/shared-key')).length, 1);
    assert.deepEqual(ranges, [{ start: 0, end: 4 }]); dispose(); await flush();
  });
  await check('subtitle preference cancels prefetch and resumes the selected track', async () => {
    const video = new Video(), engine = new Engine(); let visible = true, ranges = [];
    const text = parse(`#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\n${proxy('selected.vtt')}\n#EXT-X-ENDLIST\n`);
    engine.subtitleTracks = [{ id: 0, url: proxy('selected.m3u8'), groupId: 'text', details: text }]; engine.subtitleTrack = 0;
    const dispose = attachPlaybackCache(engine, video, () => active, value => ranges = value, undefined, () => visible);
    engine.details(plain); await tick(500);
    mode = 'hold'; engine.callbacks.get(Hls.Events.SUBTITLE_TRACK_SWITCH)(Hls.Events.SUBTITLE_TRACK_SWITCH, { id: 0, url: proxy('selected.m3u8'), groupId: 'text' });
    await tick(500); const old = downloads.at(-1).id; assert.equal(downloads.at(-1).url, 'https://media.test/selected.vtt'); assert.deepEqual(ranges, []);
    visible = false; video.dispatchEvent(new Event('subtitle-preference')); await tick(500);
    assert(stops.includes(old)); const count = downloads.length; await tick(3000); assert.equal(downloads.length, count); assert.deepEqual(ranges, [{ start: 0, end: 12 }]);
    mode = 'success'; visible = true; video.dispatchEvent(new Event('subtitle-preference')); await tick(500);
    assert(cached.has('https://media.test/selected.vtt')); assert.deepEqual(ranges, [{ start: 0, end: 4 }]); dispose(); await flush();
  });
  await check('enabling subtitles mid-video starts at the current segment', async () => {
    const video = new Video(), engine = new Engine(); let visible = false;
    const text = parse(`#EXTM3U\n#EXT-X-TARGETDURATION:4\n${[0, 1, 2].map(n => `#EXTINF:4,\n${proxy(`mid-${n}.vtt`)}`).join('\n')}\n#EXT-X-ENDLIST\n`);
    engine.subtitleTrack = -1; engine.subtitleTracks = [{ id: 0, groupId: 'text', url: proxy('mid.m3u8') }];
    const dispose = attachPlaybackCache(engine, video, () => active, () => {}, undefined, () => visible);
    engine.details(plain); await tick(500); assert(!downloads.some(item => item.url.endsWith('.vtt')));
    video.currentTime = 9; video.dispatchEvent(new Event('seeking')); visible = true; engine.subtitleTrack = 0;
    video.dispatchEvent(new Event('subtitle-preference'));
    engine.callbacks.get(Hls.Events.SUBTITLE_TRACK_LOADED)(Hls.Events.SUBTITLE_TRACK_LOADED, { id: 0, groupId: 'text', track: engine.subtitleTracks[0], details: text });
    await tick(500); assert(cached.has('https://media.test/mid-2.vtt'));
    assert(!cached.has('https://media.test/mid-0.vtt') && !cached.has('https://media.test/mid-1.vtt'));
    visible = false; video.dispatchEvent(new Event('subtitle-preference')); await tick(500);
    const before = downloads.length; visible = true; video.dispatchEvent(new Event('subtitle-preference'));
    engine.subtitleTracks[0].details = text;
    engine.callbacks.get(Hls.Events.SUBTITLE_TRACK_SWITCH)(Hls.Events.SUBTITLE_TRACK_SWITCH, { id: 0, groupId: 'text', url: proxy('mid.m3u8') });
    await tick(500); assert.equal(downloads.length, before, 'cached current subtitles are reused'); dispose(); await flush();
  });
  await check('switching audio rejects the old track late response', async () => {
    const video = new Video(), engine = new Engine();
    const track = name => parse(`#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\n${proxy(name)}\n#EXT-X-ENDLIST\n`);
    const old = track('audio-old'), next = track('audio-new');
    const dispose = attachPlaybackCache(engine, video, () => active, () => {});
    engine.details(plain); await tick(500); engine.audioTrack = 0; engine.audioTracks = [];
    mode = 'hold'; engine.callbacks.get(Hls.Events.AUDIO_TRACK_SWITCHING)(Hls.Events.AUDIO_TRACK_SWITCHING, { id: 0, groupId: 'old', url: proxy('old.m3u8'), details: old }); await tick(500);
    const oldSession = downloads.at(-1).id;
    engine.callbacks.get(Hls.Events.AUDIO_TRACK_SWITCHING)(Hls.Events.AUDIO_TRACK_SWITCHING, { id: 0, groupId: 'new', url: proxy('new.m3u8') });
    engine.callbacks.get(Hls.Events.AUDIO_TRACK_LOADED)(Hls.Events.AUDIO_TRACK_LOADED, { id: 0, groupId: 'old', track: { url: proxy('old.m3u8') }, details: old });
    mode = 'success'; engine.callbacks.get(Hls.Events.AUDIO_TRACK_LOADED)(Hls.Events.AUDIO_TRACK_LOADED, { id: 0, groupId: 'new', track: { url: proxy('new.m3u8') }, details: next });
    await tick(500); assert(stops.includes(oldSession)); assert(cached.has('https://media.test/audio-new')); assert(!cached.has('https://media.test/audio-old')); dispose(); await flush();
  });
  await check('native HLS follows the selected audio and subtitle groups', async () => {
    let visible = false, ranges = [];
    const fetched = [];
    const master = `#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="audio",NAME="English",LANGUAGE="en",DEFAULT=YES,URI="${proxy('native-audio.m3u8')}"\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="other",NAME="Other",LANGUAGE="es",URI="${proxy('unused-audio.m3u8')}"\n#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID="text",NAME="Chinese",LANGUAGE="zh",DEFAULT=YES,URI="${proxy('native-text.m3u8')}"\n#EXT-X-STREAM-INF:BANDWIDTH=800000,AUDIO="audio",SUBTITLES="text"\n${proxy('native-main.m3u8')}\n`;
    const media = name => `#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\n${proxy(name)}\n#EXT-X-ENDLIST\n`;
    globalThis.fetch = async request => {
      fetched.push(request);
      const target = new URL(request).searchParams.get('u') || '';
      const body = target.endsWith('native-main.m3u8') ? media('native-video') : target.endsWith('native-audio.m3u8') ? media('native-audio') : target.endsWith('native-text.m3u8') ? media('native-text.vtt') : master;
      return { ok: true, text: async () => body };
    };
    globalThis.cacheFixtureApi.playbackCacheObserved = async urls => urls.length ? 0 : null;
    const video = new Video(); const dispose = attachPlaybackCache(null, video, () => active, value => ranges = value, 'http://hg.localhost/hls?u=master', () => visible);
    await tick(5000);
    assert(cached.has('https://media.test/native-audio')); assert(cached.has('https://media.test/native-video'));
    assert(!fetched.some(url => url.includes(encodeURIComponent('native-text.m3u8'))));
    assert(!fetched.some(url => url.includes(encodeURIComponent('unused-audio.m3u8'))));
    visible = true; video.dispatchEvent(new Event('subtitle-preference')); await tick(500);
    assert.deepEqual(ranges, [], 'pending selected subtitles are not shown as complete');
    await tick(4500); assert(cached.has('https://media.test/native-text.vtt'));
    visible = false; video.dispatchEvent(new Event('subtitle-preference')); await tick(500); visible = true; video.dispatchEvent(new Event('subtitle-preference')); await tick(3000);
    assert(cached.has('https://media.test/native-text.vtt')); dispose(); await flush();
  });
  await check('unsupported direct origin has bounded probe retries', async () => {
    let probes = 0;
    const original = globalThis.cacheFixtureApi.playbackCacheProbe;
    globalThis.cacheFixtureApi.playbackCacheProbe = async () => { probes++; throw new Error('no strong validator'); };
    const dispose = attachDirectCache(proxy('unsupported.mp4'), new Video(), () => active, () => {});
    await tick(60000); assert.equal(probes, 3); assert.equal(downloads.length, 0);
    dispose(); await flush(); globalThis.cacheFixtureApi.playbackCacheProbe = original;
  });
  await check('clear cancels a direct probe before prefetch', async () => {
    let rejectProbe; globalThis.cacheFixtureApi.playbackCacheProbe = async (_url, id) => new Promise((_, reject) => { rejectProbe = reject; pending.set(id, reject); });
    const video = new Video(); const dispose = attachDirectCache(proxy('movie.mp4'), video, () => active, () => {});
    await tick(500); assert(rejectProbe); config.max_mib = 0; config.revision++; window.dispatchEvent(new Event('playback-cache-change')); await tick(500);
    assert(stops.length > 0); assert.equal(downloads.length, 0); dispose(); await flush();
  });
  await check('zero capacity does not paint the player buffer as cache', async () => {
    config.max_mib = 0;
    const video = new Video(), engine = new Engine(); let ranges = [{ start: 1, end: 2 }];
    video.buffered = { length: 1, start: () => 0, end: () => 30 };
    const dispose = attachPlaybackCache(engine, video, () => active, value => ranges = value);
    engine.details(plain); video.dispatchEvent(new Event('progress')); await tick(5000);
    assert.equal(downloads.length, 0); assert.deepEqual(ranges, []);
    dispose(); await flush();
  });
  await check('turning capacity to zero clears an existing cache indicator', async () => {
    const video = new Video(), engine = new Engine(); let ranges = [];
    const dispose = attachPlaybackCache(engine, video, () => active, value => ranges = value);
    engine.details(plain); await tick(500); assert.deepEqual(ranges, [{ start: 0, end: 12 }]);
    video.buffered = { length: 1, start: () => 0, end: () => 30 };
    config.max_mib = 0; config.revision++; cached.clear(); window.dispatchEvent(new Event('playback-cache-change'));
    video.dispatchEvent(new Event('progress')); await tick(5000);
    assert.equal(downloads.length, 3); assert.deepEqual(ranges, []);
    dispose(); await flush();
  });
  await check('direct playback at zero does not probe or show buffer', async () => {
    config.max_mib = 0; let probes = 0;
    const original = globalThis.cacheFixtureApi.playbackCacheProbe;
    globalThis.cacheFixtureApi.playbackCacheProbe = async () => { probes++; return { total: 1024, tag: 'etag' }; };
    const video = new Video(); let ranges = [{ start: 0, end: 1 }];
    video.buffered = { length: 1, start: () => 0, end: () => 40 };
    const dispose = attachDirectCache(proxy('movie.mp4'), video, () => active, value => ranges = value);
    video.dispatchEvent(new Event('progress')); await tick(5000);
    assert.equal(probes, 0); assert.equal(downloads.length, 0); assert.deepEqual(ranges, []);
    dispose(); await flush(); globalThis.cacheFixtureApi.playbackCacheProbe = original;
  });
  console.log(`Playback cache checks: ${passed} passed`);
} finally { Date.now = realNow; await rm(temporary, { recursive: true, force: true }); }

import assert from 'node:assert/strict';
import { setTimeout } from 'node:timers/promises';

// Playwright's waitForFunction polls synchronous predicates; poll the backend here.
async function until(check, message, timeout = 15000) {
  const deadline = performance.now() + timeout;
  do {
    if (await check()) return;
    await setTimeout(50);
  } while (performance.now() < deadline);
  assert.fail(message);
}

export const checks = {
  async replay(page, h) {
    await h.loadResumeFixture(page);
    await page.evaluate(() => window.playerSeekFixture.record(991, 8, 85, 90));
    await h.openCategoryVideo(page); await h.expectPlayback(page, 8, 85);
    await h.clickProgress(page, .999);
    await page.waitForFunction(() => document.querySelector('video').ended);
    const rewindAndPlay = async () => {
      const slider = page.getByRole('slider', { name: '播放进度' });
      await slider.scrollIntoViewIfNeeded();
      const rect = await slider.boundingBox(); assert(rect);
      await page.mouse.move(rect.x + rect.width * .999, rect.y + rect.height / 2);
      await page.mouse.down();
      await page.mouse.move(rect.x + rect.width * .5, rect.y + rect.height / 2, { steps: 8 });
      await page.mouse.up();
      await page.waitForFunction(() => !document.querySelector('video').seeking && Math.abs(document.querySelector('video').currentTime - 45) < 1);
      await page.locator('video').evaluate(video => video.play());
      await page.waitForFunction(() => document.querySelector('video').currentTime > 46);
      const result = await page.evaluate(() => {
        const video = document.querySelector('video'); video.pause();
        window.dispatchEvent(new Event('pagehide'));
        return { time: video.currentTime, saved: window.playerSeekFixture.getProgress(991) };
      });
      assert.equal(result.saved.ep, 8);
      assert(Math.abs(result.saved.t - result.time) < 1, `Replaying must save the new position, got ${JSON.stringify(result)}`);
      return result;
    };
    await rewindAndPlay();
    // A second completion in the same player must still be recognized before native EOF.
    await h.clickProgress(page, .999);
    await page.waitForFunction(() => {
      const video = document.querySelector('video'), saved = window.playerSeekFixture.getProgress(991);
      return video.paused && !video.ended && !video.seeking && saved.t === saved.dur;
    });
    const result = await rewindAndPlay();
    await h.backFromPlayer(page); await h.openCategoryVideo(page);
    await h.expectPlayback(page, 8, Math.floor(result.time));
    return { same_player_replay: true, drag_after_ended: true, repeated_completion: true, replay_position_persisted: true };
  },

  async interrupted(page, h) {
    await h.loadResumeFixture(page);
    let release;
    const gate = new Promise(resolve => { release = resolve; });
    let held = 0;
    const retained = [];
    await page.route('**/seg?*', async route => {
      if (!new URL(route.request().url()).searchParams.get('u')?.endsWith('seg017.ts')) return route.continue();
      held++; await gate; await route.continue().catch(() => {});
    });
    try {
      await h.openCategoryVideo(page);
      await page.waitForFunction(() => document.querySelector('video')?.seeking && window.playerSeekFixture.seeks.some(time => time >= 35));
      retained.push(await page.evaluate(() => window.playerSeekFixture.getProgress(991)));
      await h.clickProgress(page, .65);
      await h.expectPlayback(page, 3, 58);
      release();
      await page.waitForTimeout(250);
      assert((await page.locator('video').evaluate(video => video.currentTime)) >= 58, 'The pending history seek must not override the user seek');
    } finally { release(); await page.unroute('**/seg?*'); }
    assert(held > 0, 'Hold the target fragment after metadata has loaded');
    assert.equal(retained[0].t, 35, 'A half-finished restore must preserve the old record');

    let unblock;
    const requests = [];
    const manifests = new Promise(resolve => { unblock = resolve; });
    await page.route('**/hls?*', async route => {
      const target = new URL(new URL(route.request().url()).searchParams.get('u'));
      if (!['4', '5'].includes(target.searchParams.get('episode'))) return route.continue();
      requests.push(target.searchParams.get('episode'));
      await manifests; await route.continue().catch(() => {});
    });
    try {
      await h.pickEpisode(page, 4);
      await page.waitForFunction(() => window.playerSeekFixture.playRequests.at(-1)?.ep === 4);
      await h.pickEpisode(page, 5);
      await page.waitForFunction(() => window.playerSeekFixture.playRequests.at(-1)?.ep === 5);
      await h.pickEpisode(page, 6);
      await h.expectPlayback(page, 6, 0);
      unblock(); await page.waitForTimeout(250);
      assert.equal(await page.evaluate(() => window.playerSeekFixture.getProgress(991).ep), 6);
    } finally { unblock(); await page.unroute('**/hls?*'); }
    assert(requests.length > 0);
    return { restore_cancelled_by_user_seek: true, rapid_episode_changes: true, late_manifests_ignored: true };
  },

  async autonext(page, h) {
    await h.loadResumeFixture(page, '&mixed=1');
    await page.evaluate(() => window.playerSeekFixture.record(991, 1, 85, 90));
    await h.openCategoryVideo(page); await h.expectPlayback(page, 1, 85);
    let release;
    const gate = new Promise(resolve => { release = resolve; });
    await page.route('**/hls?*', async route => {
      const target = new URL(new URL(route.request().url()).searchParams.get('u'));
      if (target.searchParams.get('episode') !== '2') return route.continue();
      await gate; await route.continue().catch(() => {});
    });
    try {
      await h.clickProgress(page, .999);
      await page.waitForFunction(() => window.playerSeekFixture.playRequests.at(-1)?.ep === 2);
      const old = await page.evaluate(() => window.playerSeekFixture.getProgress(991));
      assert.equal(old.ep, 1); assert.equal(old.t, old.dur);
      release(); await h.expectPlayback(page, 2, 0);
      const next = await page.evaluate(() => window.playerSeekFixture.getProgress(991));
      assert(Math.abs(next.dur - 30) < 1, `New duration must come from episode 2, got ${next.dur}`);
      assert(next.t < 5);
      await h.backFromPlayer(page); await h.openCategoryVideo(page); await h.expectPlayback(page, 2, 0);
    } finally { release(); await page.unroute('**/hls?*'); }
    return { successful_autonext: true, loading_preserves_previous_episode: true, different_duration: true };
  },

  async storage(page, h) {
    await page.goto(`${h.report.ui}/tests/watch-progress.html`);
    await page.waitForFunction(() => window.watchProgressResults);
    const result = await page.evaluate(() => window.watchProgressResults);
    assert.equal(result.failed, 0, JSON.stringify(result));
    return result;
  },

  async 'cache-resume'(page, h) {
    const urls = [16, 17, 18].map(index => `${h.report.media}/vod/seg${String(index).padStart(3, '0')}.ts`);
    const sessionId = 'seed-hls-resume';
    await h.backend.command('start', { sessionId });
    try {
      for (const url of urls) assert(await h.backend.command('prefetch', { url, sessionId, protectedUrls: urls }));
    } finally { await h.backend.command('stop', { sessionId }); }
    const before = await h.backend.command('status');
    const segmentRequests = () => h.mediaRequests.filter(request => request.route === '/vod/seg017.ts').length;
    const hits = segmentRequests();
    await h.loadResumeFixture(page, `&backend=${encodeURIComponent(h.backend.origin)}&upstream=${encodeURIComponent(h.report.media)}`);
    await h.openCategoryVideo(page); await h.expectPlayback(page, 3, 35);
    await page.waitForFunction(() => window.playerSeekFixture.cacheWrites.some(entry => {
      const segment = entry.url.match(/seg(\d+)\.ts$/);
      return segment && Number(segment[1]) >= 19 && entry.time >= 35 && entry.time < 38;
    }));
    assert((await h.backend.command('status')).bytes > before.bytes, 'The real disk cache did not grow after resuming');
    assert((await page.locator('video').evaluate(video => video.currentTime)) < 38, 'Prefetch must advance before the original cached range ends');
    assert.equal(segmentRequests(), hits, 'The target fragment must be read from the Rust disk cache');
    await h.backFromPlayer(page); await h.openCategoryVideo(page); await h.expectPlayback(page, 3, 35);
    assert.equal(segmentRequests(), hits, 'Reopening must reuse the cached seek target');
    return { real_rust_cache: true, history_resume: true, advance_before_cached_edge: true, cached_target_reused: true };
  },

  async 'mp4-cache'(page, h) {
    await h.loadResumeFixture(page, `&backend=${encodeURIComponent(h.backend.origin)}&upstream=${encodeURIComponent(h.report.media)}&direct=1`);
    await h.openCategoryVideo(page); await h.expectPlayback(page, 3, 35);
    const url = `${h.report.media}/vod/video.mp4?video=991&episode=3`, sessionId = 'verify-mp4';
    await h.backend.command('start', { sessionId });
    let info;
    try { info = await h.backend.command('probe', { url, sessionId }); }
    finally { await h.backend.command('stop', { sessionId }); }
    assert(Number.isSafeInteger(info.total) && info.total > 1048576 && info.tag, 'Expected a validated MP4 spanning multiple disk blocks');
    const keys = [];
    for (let start = 0; start < info.total; start += 1048576) keys.push(`${url}\nrange=${start}-${Math.min(start + 1048576, info.total) - 1}\nvalidator=${info.tag}`);
    await until(async () => (await h.backend.command('contains', { urls: keys })).every(Boolean), 'The MP4 disk blocks were not completed');
    assert.deepEqual(await h.backend.command('pending', { url }), []);
    const resource = new URL(url).pathname + new URL(url).search;
    const transfers = () => h.mediaRequests.filter(request => request.resource === resource && request.bytes > 1 && !request.aborted);
    const ranges = transfers().map(request => request.range.match(/^bytes=(\d+)-(\d+)$/).slice(1).map(Number)).sort((a, b) => a[0] - b[0]);
    assert(ranges.length > 2);
    assert(ranges.every((range, i) => i === 0 || ranges[i - 1][1] < range[0]), 'Playback and background completion must reuse overlapping byte ranges');
    assert.equal(transfers().reduce((total, request) => total + request.bytes, 0), info.total, 'Fetch media bytes once while completing the fixed blocks');
    const requests = transfers().length;
    await h.clickProgress(page, .78); await h.expectPlayback(page, 3, 70);
    await h.backFromPlayer(page); await h.openCategoryVideo(page); await h.expectPlayback(page, 3, 70);
    assert.equal(transfers().length, requests, 'Seeking and reopening a complete MP4 cache must not download media again');
    return { decoded_mp4: true, real_rust_range_proxy: true, resume: true, fixed_blocks_completed: keys.length, media_bytes_fetched_once: info.total, seek_and_reopen_reuse: true };
  },
};

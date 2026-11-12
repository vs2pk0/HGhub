import React, { act, useMemo, useRef } from "react";
import { createRoot, type Root } from "react-dom/client";
import { AppProvider, useStore } from "../src/store";
import { useWatchProgress } from "../src/useWatchProgress";
import type { VideoHandle } from "../src/components/VideoSurface";
import { api } from "../src/api";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
api.boot = async () => ({ user: null, logged_in: false, base: "https://fixture.invalid" });
const LS = "hghub_app_v1";
const originalGet = Storage.prototype.getItem;
const originalSet = Storage.prototype.setItem;
let store: ReturnType<typeof useStore>;
let player: ReturnType<typeof useWatchProgress>;
let media: ReturnType<typeof makeMedia>;
let root: Root;
const results: { name: string; passed: boolean; error?: string }[] = [];
const assert = (condition: unknown, message: string) => { if (!condition) throw new Error(message); };
const saved = (id = 1, ep = 3, t = 35, dur = 90) => ({ id, ep, t, dur, at: Date.now() });
type Options = { id: number; active?: boolean; startEp?: number; startTime?: number };

function makeMedia() {
  const state = { time: 0, duration: 90, ready: 3, paused: false, seeking: false };
  const video = document.createElement("video");
  for (const [property, get] of Object.entries({ currentTime: () => state.time, duration: () => state.duration,
    readyState: () => state.ready, paused: () => state.paused, seeking: () => state.seeking, ended: () => state.time >= state.duration })) {
    Object.defineProperty(video, property, { configurable: true, get });
  }
  const seeks: number[] = [];
  const handle = { el: () => video, seek: (time: number) => { seeks.push(time); state.time = time; state.seeking = true; } } as VideoHandle;
  return { state, video, seeks, handle };
}
function Harness(options: Options) {
  const ref = useRef<VideoHandle | null>(null);
  player = useWatchProgress({ ...options, active: options.active ?? true, videoRef: ref });
  media = useMemo(makeMedia, [player.key]);
  ref.current = media.handle;
  return null;
}
function Probe({ options }: { options?: Options }) {
  store = useStore();
  return options ? <Harness {...options} /> : null;
}
const render = async (options?: Options) => { await act(async () => root.render(<AppProvider><Probe options={options} /></AppProvider>)); };
async function test(name: string, seed: unknown, run: () => Promise<void>) {
  try {
    localStorage.setItem(LS, typeof seed === "string" ? seed : JSON.stringify(seed));
    root = createRoot(document.querySelector("#fixture")!);
    await render(); await run();
    results.push({ name, passed: true });
  } catch (error) { results.push({ name, passed: false, error: String(error) }); }
  finally {
    Storage.prototype.getItem = originalGet;
    Storage.prototype.setItem = originalSet;
    await act(async () => root?.unmount());
  }
}

await test("legacy numeric IDs and malformed fields migrate safely", { history: [12, 0, -1,
  { id: 13, ep: "2", t: -3, dur: "invalid", at: 10 }, { id: 14, ep: 1.5, t: "6", dur: 30, at: 11 }] }, async () => {
  assert(store.getWatchProgress(12)?.ep === 1, "numeric history was lost");
  assert(store.getWatchProgress(13)?.ep === 2 && store.getWatchProgress(13)?.t === 0 && store.getWatchProgress(13)?.dur === 0, "invalid fields were not normalized");
  assert(store.getWatchProgress(14)?.ep === 1 && store.getWatchProgress(14)?.t === 6, "numeric fields were not normalized");
  assert(!store.getWatchProgress(0) && !store.getWatchProgress(-1), "invalid IDs survived");
});
await test("damaged JSON falls back to an operable store", "{broken", async () => {
  assert(store.history.length === 0 && !store.getWatchProgress(1), "corrupt JSON did not fall back");
  await act(async () => store.addHistory(1, 2, 5, 30));
  assert(JSON.parse(localStorage.getItem(LS)!).progress[1].ep === 2, "store could not recover after invalid JSON");
});
await test("denied storage access permits playback and recovers when writable", { history: [saved()] }, async () => {
  await act(async () => root.unmount());
  Storage.prototype.getItem = function (key) { if (key === LS) throw new DOMException("denied", "SecurityError"); return originalGet.call(this, key); };
  Storage.prototype.setItem = function (key, value) { if (key === LS) throw new DOMException("denied", "SecurityError"); originalSet.call(this, key, value); };
  root = createRoot(document.querySelector("#fixture")!); await render({ id: 1 });
  assert(!store.getWatchProgress(1), "denied reads did not fall back to an empty store");
  await act(async () => { media.state.time = 12; player.onDuration(90); player.onPlaying(); player.onPause(); });
  assert(store.getWatchProgress(1)?.t === 12 && store.toasts.length === 1, "denied storage prevented in-memory playback tracking");
  Storage.prototype.getItem = originalGet; Storage.prototype.setItem = originalSet;
  await act(async () => store.setMuted(false));
  assert(JSON.parse(localStorage.getItem(LS)!).progress[1].t === 12, "storage recovery did not persist the active playback");
});
await test("new progress wins duplicate IDs and an explicit clear remains clear", {
  history: [saved()], progress: { first: { ...saved(), at: 1 }, second: { ...saved(1, 4, 9), at: 2 } },
}, async () => {
  assert(store.getWatchProgress(1)?.ep === 4, "newer duplicate was lost");
  await act(async () => store.clearHistory());
  await act(async () => root.unmount());
  root = createRoot(document.querySelector("#fixture")!); await render();
  assert(!store.getWatchProgress(1) && store.history.length === 0, "cleared records came back after remount");
});
await test("quota errors preserve memory, notify once, and recover the latest state", { history: [saved(1, 3, 5)] }, async () => {
  Storage.prototype.setItem = function (key, value) { if (key === LS) throw new DOMException("full", "QuotaExceededError"); originalSet.call(this, key, value); };
  await act(async () => { store.addHistory(1, 3, 10, 90); store.addHistory(1, 3, 20, 90); store.setVolume(.4); });
  assert(store.getWatchProgress(1)?.t === 20, "quota error lost the memory state");
  assert(store.toasts.length === 1, "repeated failures flooded notifications");
  assert(JSON.parse(localStorage.getItem(LS)!).history[0].t === 5, "a failed write changed the persisted record");
  Storage.prototype.setItem = originalSet;
  await act(async () => store.setMuted(false));
  const persisted = JSON.parse(localStorage.getItem(LS)!);
  assert(persisted.progress[1].t === 20 && persisted.volume === .4 && persisted.muted === false, "recovery wrote stale state");
  Storage.prototype.setItem = function () { throw new DOMException("full", "QuotaExceededError"); };
  await act(async () => store.addHistory(1, 3, 25, 90));
  assert(store.toasts.length === 2, "a new failure after recovery should notify again");
});
await test("user seek interrupts a restore before it can replace the saved position", { history: [saved()] }, async () => {
  await render({ id: 1 });
  await act(async () => { player.onDuration(90); player.onPlaying(); player.onTime(); player.onPause(); });
  assert(store.getWatchProgress(1)?.t === 35 && media.seeks[0] === 35, "unfinished restore overwrote its history");
  await act(async () => {
    player.onSeekRequest(55, "desktop_click"); media.state.time = 55; media.state.seeking = false;
    player.onPlaying(); player.onPause();
  });
  assert(store.getWatchProgress(1)?.t === 55, "the user's target did not win");
});
await test("replay saves its new position and still accepts an early end signal", {}, async () => {
  await render({ id: 1 });
  await act(async () => {
    media.state.time = 88.7; player.onDuration(90); player.onPlaying(); player.onEnded();
  });
  assert(store.getWatchProgress(1)?.t === 90, "the near-end waiting signal did not save completion");
  await act(async () => {
    player.onSeekRequest(45, "desktop_drag"); media.state.time = 45; player.onPlaying(); player.onPause();
  });
  assert(store.getWatchProgress(1)?.t === 45, "a replay still saved the old completed state");
  await act(async () => { media.state.time = 88.7; player.onEnded(); });
  assert(store.getWatchProgress(1)?.t === 90, "the second near-end signal was lost");
});
await test("late callbacks from a previous source cannot change the new playback", { history: [saved()] }, async () => {
  await render({ id: 1 });
  await act(async () => { player.onDuration(90); media.state.seeking = false; player.onPlaying(); });
  const old = player;
  await act(async () => player.selectEpisode(4));
  await act(async () => { media.state.duration = 30; media.state.time = 5; player.onDuration(30); player.onPlaying(); });
  await act(async () => { old.onDuration(200); old.onTime(); old.onPlaying(); old.onPause(); assert(old.onEnded() === false, "late ended could advance the new episode"); });
  const current = store.getWatchProgress(1)!;
  assert(current.ep === 4 && current.t === 5 && current.dur === 30, "old source changed the new episode or duration");
  await render({ id: 2 });
  await act(async () => { player.onDuration(60); media.state.duration = 60; media.state.time = 7; player.onPlaying(); old.onPause(); });
  assert(store.getWatchProgress(2)?.t === 7 && store.getWatchProgress(1)?.ep === 4, "late source data crossed video IDs");
});
for (const position of [86.99, 87, 87.01, 90, 120]) {
  await test(`resume boundary at ${position} seconds keeps the episode`, { history: [saved(1, 3, position)] }, async () => {
    await render({ id: 1 });
    await act(async () => player.onDuration(90));
    assert(player.episode === 3, "tail policy changed the episode");
    assert(position < 87 ? media.seeks[0] === position : media.seeks.length === 0, "wrong side of the three-second boundary");
    assert(store.getWatchProgress(1)?.t === position, "metadata alone replaced a record");
  });
}

const report = { passed: results.filter(result => result.passed).length, failed: results.filter(result => !result.passed).length, results };
document.querySelector("#results")!.textContent = JSON.stringify(report, null, 2);
Object.assign(window, { watchProgressResults: report });

import { useEffect, useLayoutEffect, useMemo, useRef, useState, type RefObject } from "react";
import type { VideoHandle } from "./components/VideoSurface";
import { useStore } from "./store";

type Playback = { id: number; ep: number; time: number; version: number };
type Session = {
  playback: Playback;
  video?: HTMLVideoElement;
  resume: number | null;
  assigned: boolean;
  writer?: number;
  time: number;
  duration: number;
  savedAt: number;
  ended: boolean;
};

/** A playback instance owns its callbacks and writer, including during source cleanup. */
export function useWatchProgress({ id, active, videoRef, startEp, startTime }: {
  id: number; active: boolean; videoRef: RefObject<VideoHandle | null>; startEp?: number; startTime?: number;
}) {
  const { getWatchProgress, claimWatchProgress, addHistory } = useStore();
  const resolve = (id: number, ep?: number, time?: number): Omit<Playback, "version"> => {
    const saved = getWatchProgress(id);
    const episode = typeof ep === "number" && Number.isSafeInteger(ep) && ep > 0 ? ep : saved?.ep ?? 1;
    const position = time ?? (saved?.ep === episode ? saved.t : 0);
    return { id, ep: episode, time: Number.isFinite(position) && position > 0 ? position : 0 };
  };
  const [playback, setPlayback] = useState<Playback>(() => ({ ...resolve(id, startEp, startTime), version: 0 }));
  // Resolve before committing the new video so no request for episode 1 can overwrite its progress.
  if (playback.id !== id) setPlayback({ ...resolve(id), version: playback.version + 1 });
  const session = useMemo<Session>(() => ({
    playback, resume: playback.time > 0 ? playback.time : null, assigned: false,
    time: 0, duration: 0, savedAt: 0, ended: false,
  }), [playback]);
  const current = useRef(session); current.current = session;
  const isActive = useRef(active); isActive.current = active;

  const sample = (s: Session) => {
    const video = s.video;
    if (!video || video.readyState < 2 || video.seeking || s.resume !== null) return;
    if (Number.isFinite(video.duration) && video.duration > 0) s.duration = video.duration;
    if (Number.isFinite(video.currentTime)) s.time = Math.max(0, video.currentTime);
    // VideoSurface 可在片尾 1.5 秒内提前报结束，回拖离开此区间后才清除完成标记。
    if (s.ended && !video.ended && s.time < s.duration - Math.min(1.5, s.duration / 2)) s.ended = false;
  };
  const save = (s: Session) => {
    if (s.writer === undefined || s.resume !== null) return;
    sample(s);
    addHistory(s.playback.id, s.playback.ep, s.ended ? s.duration : s.time, s.duration, s.writer);
    s.savedAt = Date.now();
  };
  const restored = (s: Session) => {
    if (s.resume === null) return true;
    const video = s.video;
    if (s.assigned && video && !video.seeking && Math.abs(video.currentTime - s.resume) < 1) {
      s.resume = null;
      return true;
    }
    return false;
  };
  const playing = () => {
    const video = session.video ??= videoRef.current?.el() ?? undefined;
    if (current.current !== session || !isActive.current || !video || video.paused || video.seeking || video.readyState < 2 || !restored(session)) return;
    session.writer = claimWatchProgress(playback.id);
    save(session);
  };

  useLayoutEffect(() => {
    return () => save(session);
  }, [session]);
  useLayoutEffect(() => { if (!active) save(session); }, [active, session]);
  useEffect(() => {
    const flush = () => save(session);
    const visibility = () => { if (document.visibilityState === "hidden") flush(); };
    window.addEventListener("pagehide", flush);
    document.addEventListener("visibilitychange", visibility);
    return () => { window.removeEventListener("pagehide", flush); document.removeEventListener("visibilitychange", visibility); };
  }, [session]);

  return {
    episode: playback.ep,
    key: `${playback.id}:${playback.ep}:${playback.version}`,
    selectEpisode: (ep: number) => {
      if (!Number.isSafeInteger(ep) || ep < 1) return;
      save(session);
      setPlayback(previous => ({ id, ep, time: 0, version: previous.version + 1 }));
    },
    onDuration: (duration: number) => {
      if (current.current !== session || !Number.isFinite(duration) || duration <= 0) return;
      session.video ??= videoRef.current?.el() ?? undefined;
      session.duration = duration;
      if (!isActive.current || session.resume === null || session.assigned) return;
      if (session.resume >= duration - 3) { session.resume = null; return 0; }
      const target = session.resume;
      session.assigned = true;
      videoRef.current?.seek(target, "history_resume");
      return target;
    },
    onTime: () => {
      if (current.current !== session || !restored(session)) return false;
      sample(session);
      const video = session.video;
      if (isActive.current && video && !video.paused && !video.seeking && video.readyState >= 2) {
        if (session.writer === undefined) playing();
        else if (Date.now() - session.savedAt >= 5000) save(session);
      }
      return true;
    },
    onPlaying: playing,
    onSeekRequest: (_time: number, source: string) => {
      if (current.current === session && source !== "history_resume") session.resume = null;
    },
    onPause: () => save(session),
    onEnded: () => {
      if (current.current !== session || !isActive.current) return false;
      session.ended = true;
      save(session);
      return true;
    },
  };
}

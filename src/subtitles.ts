import Hls from "hls.js";

/** Reapply the saved visibility when native/HLS tracks arrive or change after source loading. */
export function attachSubtitles(video: HTMLVideoElement, hls: Hls | null, enabled: () => boolean) {
  let applying = false;
  let selected: TextTrack | undefined;
  let selectedHls: { name: string; lang?: string } | undefined;
  const apply = () => {
    if (applying) return;
    applying = true;
    try {
      const show = enabled();
      if (hls) {
        if (hls.subtitleDisplay !== show) hls.subtitleDisplay = show;
        if (!show && hls.subtitleTrack >= 0) {
          const track = hls.subtitleTracks[hls.subtitleTrack];
          if (track) selectedHls = { name: track.name, lang: track.lang };
          hls.subtitleTrack = -1;
        }
        if (show && hls.subtitleTrack < 0 && hls.subtitleTracks.length) {
          const previous = selectedHls ? hls.subtitleTracks.findIndex(track => track.name === selectedHls!.name && track.lang === selectedHls!.lang) : -1;
          const preferred = previous >= 0 ? previous : hls.subtitleTracks.findIndex(track => track.default);
          hls.subtitleTrack = preferred >= 0 ? preferred : 0;
        }
      }
      const tracks = Array.from(video.textTracks).filter(track => track.kind === "subtitles" || track.kind === "captions");
      if (!show) {
        const showing = tracks.find(track => track.mode === "showing");
        if (showing) selected = showing;
        for (const track of tracks) if (track.mode === "showing") track.mode = "hidden";
      } else {
        selected = tracks.find(track => track.mode === "showing")
          ?? (selected && tracks.includes(selected) ? selected : undefined)
          ?? tracks.find(track => track.mode === "hidden") ?? tracks[0];
        for (const track of tracks) {
          const mode = track === selected ? "showing" : "disabled";
          if (track.mode !== mode) track.mode = mode;
        }
      }
    } finally { applying = false; }
  };
  const events = [Hls.Events.MEDIA_ATTACHED, Hls.Events.MANIFEST_PARSED, Hls.Events.SUBTITLE_TRACKS_UPDATED, Hls.Events.SUBTITLE_TRACK_SWITCH, Hls.Events.SUBTITLE_TRACK_LOADED] as const;
  for (const event of events) hls?.on(event, apply);
  video.textTracks.addEventListener("addtrack", apply);
  video.textTracks.addEventListener("change", apply);
  video.addEventListener("loadedmetadata", apply);
  video.addEventListener("subtitle-preference", apply);
  apply();
  return () => {
    for (const event of events) hls?.off(event, apply);
    video.textTracks.removeEventListener("addtrack", apply);
    video.textTracks.removeEventListener("change", apply);
    video.removeEventListener("loadedmetadata", apply);
    video.removeEventListener("subtitle-preference", apply);
  };
}

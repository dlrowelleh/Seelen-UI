import { cx } from "libs/ui/react/utils/styling.ts";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useMemo, useRef } from "preact/hooks";

import type { DefinedWallProps } from "../types";
import { getPlaybackRate, getWallpaperStyles } from "../utils.ts";
import cs from "../index.module.css";

const MAX_RETRIES = 3;
const WAITING_TIMEOUT_MS = 3000;
const STALL_CHECK_INTERVAL_MS = 5000;

export function VideoWallpaper({ definition, config, muted, paused, onLoad }: DefinedWallProps) {
  const ref = useRef<HTMLVideoElement>(null);
  const waitingTimeoutRef = useRef<ReturnType<typeof setTimeout>>();
  const retryTimeoutRef = useRef<ReturnType<typeof setTimeout>>();
  const savedTimeRef = useRef<number | null>(null);
  const retryCountRef = useRef(0);
  const lastTimeUpdateRef = useRef(0);
  const isLoadedRef = useRef(false);
  const attachedSrcRef = useRef("");

  const videoSrc = useMemo(
    () => convertFileSrc(definition.metadata.path + "\\" + definition.filename!),
    [definition.metadata.path, definition.filename],
  );

  // A paused video can retain its decoder and frame buffers. Detach its source to
  // release those resources, then restore the playback position after metadata loads.
  useEffect(() => {
    const video = ref.current;
    if (!video) return;

    if (paused) {
      if (attachedSrcRef.current === videoSrc) {
        savedTimeRef.current = video.currentTime;
      }
      video.pause();
      if (waitingTimeoutRef.current) clearTimeout(waitingTimeoutRef.current);
      if (retryTimeoutRef.current) clearTimeout(retryTimeoutRef.current);
      waitingTimeoutRef.current = undefined;
      retryTimeoutRef.current = undefined;
      video.removeAttribute("src");
      video.load();
      attachedSrcRef.current = "";
      isLoadedRef.current = false;
      return;
    }

    if (attachedSrcRef.current !== videoSrc) {
      if (attachedSrcRef.current && attachedSrcRef.current !== videoSrc) {
        savedTimeRef.current = null;
      }
      attachedSrcRef.current = videoSrc;
      retryCountRef.current = 0;
      isLoadedRef.current = false;
      video.src = videoSrc;
      video.load();
    } else if (video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA) {
      video.play().catch((err) => console.error("Failed to play video:", err));
    }
  }, [paused, videoSrc]);

  useEffect(() => {
    if (paused) return;
    const interval = setInterval(() => {
      const video = ref.current;
      if (!video || !isLoadedRef.current) return;
      const currentTime = video.currentTime;
      if (
        currentTime === lastTimeUpdateRef.current &&
        video.readyState < HTMLMediaElement.HAVE_FUTURE_DATA &&
        retryCountRef.current < MAX_RETRIES
      ) {
        retryCountRef.current++;
        video.load();
      }
      lastTimeUpdateRef.current = currentTime;
    }, STALL_CHECK_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [paused]);

  useEffect(() => {
    return () => {
      if (waitingTimeoutRef.current) clearTimeout(waitingTimeoutRef.current);
      if (retryTimeoutRef.current) clearTimeout(retryTimeoutRef.current);
      if (ref.current) {
        ref.current.pause();
        ref.current.removeAttribute("src");
        ref.current.load();
      }
    };
  }, []);

  const handleWaiting = () => {
    if (paused) return;
    if (waitingTimeoutRef.current) clearTimeout(waitingTimeoutRef.current);
    waitingTimeoutRef.current = setTimeout(() => {
      waitingTimeoutRef.current = undefined;
      const video = ref.current;
      if (!video || paused || retryCountRef.current >= MAX_RETRIES) return;
      retryCountRef.current++;
      const currentTime = video.currentTime;
      video.load();
      if (currentTime > 0) savedTimeRef.current = currentTime;
    }, WAITING_TIMEOUT_MS);
  };

  const handlePlaying = () => {
    if (waitingTimeoutRef.current) clearTimeout(waitingTimeoutRef.current);
    waitingTimeoutRef.current = undefined;
    retryCountRef.current = 0;
  };

  const handleStalled = () => {
    const video = ref.current;
    if (!video || paused || retryCountRef.current >= MAX_RETRIES) return;
    if (video.readyState >= HTMLMediaElement.HAVE_FUTURE_DATA) return;
    retryCountRef.current++;
    savedTimeRef.current = video.currentTime;
    video.load();
  };

  const handleCanPlay = () => {
    const video = ref.current;
    if (!video || paused) return;
    if (savedTimeRef.current !== null) {
      const time = savedTimeRef.current;
      savedTimeRef.current = null;
      if (Number.isFinite(time) && time > 0) video.currentTime = time;
    }
    video.play().catch((err) => console.error("Failed to play video on canplay:", err));
  };

  const handleLoadedMetadata = () => {
    if (paused) return;
    isLoadedRef.current = true;
    onLoad?.();
  };

  const handleError = (e: Event) => {
    if (paused) return;
    const error = (e.target as HTMLVideoElement).error;
    if (!error) return;
    console.error("Video error:", { code: error.code, message: error.message, src: videoSrc });
    if (error.code === MediaError.MEDIA_ERR_NETWORK && retryCountRef.current < MAX_RETRIES) {
      retryCountRef.current++;
      retryTimeoutRef.current = setTimeout(() => {
        retryTimeoutRef.current = undefined;
        const video = ref.current;
        if (!video || paused) return;
        video.load();
      }, 1000);
    }
  };

  const handleTimeUpdate = () => {
    if (ref.current && !paused) lastTimeUpdateRef.current = ref.current.currentTime;
  };

  return (
    <video
      id={definition.id}
      className={cx(cs.wallpaper, "wallpaper")}
      style={getWallpaperStyles(config)}
      ref={ref}
      controls={false}
      muted={muted || config.muted}
      loop
      playsInline
      disableRemotePlayback
      preload={paused ? "none" : "metadata"}
      playbackRate={getPlaybackRate(config.playbackSpeed)}
      onLoadedMetadata={handleLoadedMetadata}
      onWaiting={handleWaiting}
      onPlaying={handlePlaying}
      onStalled={handleStalled}
      onCanPlay={handleCanPlay}
      onError={handleError}
      onTimeUpdate={handleTimeUpdate}
    />
  );
}

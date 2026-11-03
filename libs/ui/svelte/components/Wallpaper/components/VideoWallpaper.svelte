<script lang="ts">
  import { convertFileSrc } from "@tauri-apps/api/core";
  import { onDestroy } from "svelte";
  import type { DefinedWallProps } from "../types";
  import { getPlaybackRate, getWallpaperStyles } from "../utils";

  let { definition, config, muted, paused, onLoad }: DefinedWallProps = $props();

  const MAX_RETRIES = 3;
  const WAITING_TIMEOUT_MS = 3000;
  const RETRY_RESET_MS = 15000;

  let videoElement: HTMLVideoElement | undefined = $state();
  let waitingTimeout: ReturnType<typeof setTimeout> | undefined;
  let retryResetTimeout: ReturnType<typeof setTimeout> | undefined;

  let retryCount = 0;
  let isLoaded = false;
  let isReloading = false;
  let pendingSeekTime: number | null = null;
  let attachedSrc = "";
  let lastVideoSrc = "";

  const videoSrc = $derived(convertFileSrc(definition.metadata.path + "\\" + definition.filename!));

  function clearRecoveryTimers() {
    if (waitingTimeout) {
      clearTimeout(waitingTimeout);
      waitingTimeout = undefined;
    }
    if (retryResetTimeout) {
      clearTimeout(retryResetTimeout);
      retryResetTimeout = undefined;
    }
  }

  function doReload() {
    if (!videoElement || isReloading || paused) return;
    isReloading = true;
    pendingSeekTime = videoElement.currentTime > 0 ? videoElement.currentTime : null;
    videoElement.load();
  }

  function tryRecover(source: string) {
    if (paused || !videoElement) return;
    if (retryCount < MAX_RETRIES) {
      console.debug(`Video recovery [${source}], attempt ${retryCount + 1}/${MAX_RETRIES}`);
      retryCount++;
      doReload();
    } else {
      scheduleRetryReset();
    }
  }

  function scheduleRetryReset() {
    if (retryResetTimeout || paused) return;
    retryResetTimeout = setTimeout(() => {
      retryResetTimeout = undefined;
      retryCount = 0;
      if (!paused) doReload();
    }, RETRY_RESET_MS);
  }

  // Detaching the source releases idle decoder resources; resume restores the saved position.
  $effect(() => {
    const shouldPause = paused;
    const source = videoSrc;
    if (!videoElement) return;

    if (lastVideoSrc && lastVideoSrc !== source) pendingSeekTime = null;
    lastVideoSrc = source;

    if (shouldPause) {
      if (attachedSrc === source) pendingSeekTime = videoElement.currentTime;
      videoElement.pause();
      clearRecoveryTimers();
      videoElement.removeAttribute("src");
      videoElement.load();
      attachedSrc = "";
      isLoaded = false;
      isReloading = false;
      return;
    }

    if (attachedSrc !== source) {
      clearRecoveryTimers();
      retryCount = 0;
      isLoaded = false;
      isReloading = false;
      attachedSrc = source;
      videoElement.src = source;
      videoElement.load();
      return;
    }

    if (videoElement.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA) {
      videoElement.play().catch((err) => console.error("Failed to play video:", err));
    }
  });

  function handleWaiting() {
    if (paused) return;
    if (waitingTimeout) clearTimeout(waitingTimeout);
    waitingTimeout = setTimeout(() => {
      waitingTimeout = undefined;
      tryRecover("waiting");
    }, WAITING_TIMEOUT_MS);
  }

  function handlePlaying() {
    if (waitingTimeout) {
      clearTimeout(waitingTimeout);
      waitingTimeout = undefined;
    }
    if (retryResetTimeout) {
      clearTimeout(retryResetTimeout);
      retryResetTimeout = undefined;
    }
    retryCount = 0;
    isReloading = false;
  }

  function handleStalled() {
    if (!videoElement || paused) return;
    if (videoElement.readyState >= HTMLMediaElement.HAVE_FUTURE_DATA) return;
    tryRecover("stalled");
  }

  function handleCanPlay() {
    isReloading = false;
    if (!videoElement || paused) return;
    if (pendingSeekTime !== null) {
      const seekTime = pendingSeekTime;
      pendingSeekTime = null;
      if (Number.isFinite(seekTime) && seekTime > 0) videoElement.currentTime = seekTime;
    }
    videoElement.play().catch((err) => console.error("Failed to play video on canplay:", err));
  }

  function handleLoadedMetadata() {
    if (paused) return;
    isLoaded = true;
    onLoad?.();
  }

  function handleError(e: Event) {
    isReloading = false;
    if (paused) return;
    const error = (e.target as HTMLVideoElement).error;
    if (!error) return;

    console.error("Video error:", { code: error.code, message: error.message, src: videoSrc });
    if (error.code === MediaError.MEDIA_ERR_NETWORK) {
      setTimeout(() => tryRecover("network-error"), 1000);
    } else {
      scheduleRetryReset();
    }
  }

  onDestroy(() => {
    clearRecoveryTimers();
    if (videoElement) {
      videoElement.pause();
      videoElement.removeAttribute("src");
      videoElement.load();
    }
  });

  $effect(() => {
    if (videoElement) videoElement.playbackRate = getPlaybackRate(config.playbackSpeed);
  });
</script>

<video
  bind:this={videoElement}
  id={definition.id}
  class="wallpaper"
  style={getWallpaperStyles(config)}
  crossOrigin="anonymous"
  controls={false}
  muted={muted || config.muted}
  loop
  playsinline
  disableRemotePlayback
  disablepictureinpicture
  preload={paused ? "none" : "metadata"}
  onloadedmetadata={handleLoadedMetadata}
  onwaiting={handleWaiting}
  onplaying={handlePlaying}
  onstalled={handleStalled}
  oncanplay={handleCanPlay}
  onerror={handleError}
></video>

<style>
  .wallpaper {
    position: relative;
    width: 100%;
    height: 100%;
  }
</style>

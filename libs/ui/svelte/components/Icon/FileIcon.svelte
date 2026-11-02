<script lang="ts">
  import type { ClassValue } from "svelte/elements";
  import { IconPackManager } from "@seelen-ui/lib";
  import { iconPackManager, type IconState } from "./common.svelte.ts";
  import MissingIcon from "./MissingIcon.svelte";
  import SluIconRenderer from "./SluIconRenderer.svelte";
  import { prefersDarkColorScheme } from "../../runes/DarkMode.svelte.ts";

  interface Props {
    path?: string | null;
    umid?: string | null;
    class?: ClassValue;
    lazy?: boolean;
    [key: string]: any;
  }

  let { path, umid, class: className, lazy, ...imgProps }: Props = $props();

  let mounted = { value: false };

  let previousSrc = $state<string | null>(null);
  let icon: IconState = $derived.by(() => {
    // Depend on _version to trigger reactivity when icon pack changes
    iconPackManager._version;
    const icon = iconPackManager.getIcon({ path, umid });
    if (icon) {
      return {
        src: (prefersDarkColorScheme.value ? icon.dark : icon.light) || icon.base,
        mask: icon.mask,
        isAproximatelySquare: icon.isAproximatelySquare,
      };
    }

    return { src: null, mask: null, isAproximatelySquare: false };
  });

  // Watch for src becoming null (trigger icon extraction)
  $effect(() => {
    // Wait for the asynchronously initialized icon pack before issuing extraction IPC.
    if (!iconPackManager.isReady) return undefined;

    if (!mounted.value) {
      const extract = () => {
        if (mounted.value) return;
        mounted.value = true;
        IconPackManager.requestIconExtraction({ path, umid });
      };

      // Alt+Tab can mount many cards at once. Defer their native icon extraction
      // requests until the renderer is idle so the first switcher frame stays responsive.
      if (lazy) {
        const idleId = requestIdleCallback(extract, { timeout: 1000 });
        return () => cancelIdleCallback(idleId);
      }

      extract();
    }

    // Trigger icon extraction when src goes from non-null to null
    if (previousSrc !== null && icon.src === null) {
      IconPackManager.requestIconExtraction({ path, umid });
    }
    previousSrc = icon.src;
    return undefined;
  });
</script>

{#if icon.src}
  <SluIconRenderer
    {...imgProps}
    state={icon}
    class={className}
    {lazy}
    data-path={path ?? undefined}
    data-umid={umid ?? undefined}
  />
{:else}
  <MissingIcon {...imgProps} class={className} />
{/if}

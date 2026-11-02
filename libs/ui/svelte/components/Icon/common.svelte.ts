import { IconPackManager } from "@seelen-ui/lib";
import type { Icon } from "@seelen-ui/lib/types";
import { prefersDarkColorScheme } from "../../runes/DarkMode.svelte";

let innerState = $state.raw<{
  _version: number;
  value: IconPackManager | null;
}>({
  _version: 0,
  value: null,
});

// Do not make every FileIcon consumer wait for the icon pack and settings IPC calls.
// Widgets can render their fallback immediately and update when the manager is ready.
void IconPackManager.create()
  .then((manager) => {
    innerState = {
      _version: innerState._version + 1,
      value: manager,
    };

    return manager.onChange(() => {
      innerState = {
        _version: innerState._version + 1,
        value: manager,
      };
    });
  })
  .catch((error) => {
    console.error("Failed to initialize icon pack manager:", error);
  });

export interface IconState {
  src: string | null;
  mask: string | null;
  isAproximatelySquare: boolean;
}

class IconPackManagerWrap {
  get isReady(): boolean {
    return innerState.value !== null;
  }

  /** Compatibility accessor. It is null until the asynchronous initialization finishes. */
  get value(): IconPackManager | null {
    return innerState.value;
  }

  get _version() {
    return innerState._version;
  }

  resolve(icon: Icon): IconState {
    return {
      src: (prefersDarkColorScheme.value ? icon.dark : icon.light) || icon.base,
      mask: icon.mask,
      isAproximatelySquare: icon.isAproximatelySquare,
    };
  }

  getIcon(args: { path?: string | null; umid?: string | null }): Icon | null {
    return innerState.value?.getIcon(args) ?? null;
  }

  getIconEntry(args: { path?: string | null; umid?: string | null }) {
    return innerState.value?.getIconEntry(args) ?? null;
  }

  getCustomIconEntry(name: string) {
    return innerState.value?.getCustomIconEntry(name) ?? null;
  }

  getCustomIcon(name: string): Icon | null {
    return innerState.value?.getCustomIcon(name) ?? null;
  }

  getMissingIcon(): Icon | null {
    return innerState.value?.getMissingIcon() ?? null;
  }
}

export const iconPackManager = new IconPackManagerWrap();

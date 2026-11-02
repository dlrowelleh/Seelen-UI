import type {
  Icon as IIcon,
  IconPack,
  IconPack as IIconPack,
  IconPackEntry,
  SluCmdArgumentMap,
} from "@seelen-ui/types";
import { List } from "../utils/List.ts";
import { newFromInvoke, newOnEvent } from "../utils/State.ts";
import { invoke, SeelenCommand, SeelenEvent, type UnSubscriber } from "../handlers/mod.ts";
import { Settings } from "./settings/mod.ts";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { convertFileSrc } from "@tauri-apps/api/core";

type GetIconArgs = Extract<SluCmdArgumentMap, { [SeelenCommand.GetIcon]: unknown }>[SeelenCommand.GetIcon];

type UniqueIconEntry = Extract<IconPackEntry, { type: "unique" }>;
type SharedIconEntry = Extract<IconPackEntry, { type: "shared" }>;
type CustomIconEntry = Extract<IconPackEntry, { type: "custom" }>;

interface IconPackLookupIndex {
  uniqueByUmid: Map<string, UniqueIconEntry>;
  uniqueByPath: Map<string, UniqueIconEntry>;
  uniqueByFilename: Map<string, UniqueIconEntry>;
  sharedByExtension: Map<string, SharedIconEntry>;
  customByKey: Map<string, CustomIconEntry>;
}

export class IconPackList extends List<IIconPack> {
  static getAsync(): Promise<IconPackList> {
    return newFromInvoke(this, SeelenCommand.StateGetIconPacks);
  }

  static onChange(cb: (payload: IconPackList) => void): Promise<UnSubscriber> {
    return newOnEvent(cb, this, SeelenEvent.StateIconPacksChanged);
  }
}

/**
 * Class helper to allow easy use of icon packs
 */
export class IconPackManager {
  private callbacks: Set<() => void> = new Set();
  private unlisteners: UnlistenFn[] = [];
  private isListeningForChanges = false;

  /// Active icon packs in priority order, plus precomputed lookup maps so every FileIcon
  /// does not linearly scan thousands of entries during rendering.
  private activeIconPacks: IconPack[] = [];
  private activeIconIndices: IconPackLookupIndex[] = [];

  protected constructor(
    protected _availableIconPacks: IconPack[],
    protected _activeIconPackIds: string[],
  ) {}

  get iconPacks(): IconPack[] {
    return this._availableIconPacks;
  }

  get activeIconPackIds(): string[] {
    return this._activeIconPackIds;
  }

  protected resolveAvailableIcons(): void {
    for (const pack of this._availableIconPacks) {
      const path = `${pack.metadata.path}`;

      if (pack.missing) {
        pack.missing = resolveIcon(path, pack.missing);
      }

      for (const entry of pack.entries) {
        if (entry.type === "unique") {
          entry.path = entry.path?.toLowerCase() || null;
        }

        if (entry.type === "shared") {
          entry.extension = entry.extension.toLowerCase();
        }

        if (entry.icon) {
          entry.icon = resolveIcon(path, entry.icon);
        }
      }
    }
  }

  protected cacheActiveIconPacks(): void {
    this.activeIconPacks = [];
    this.activeIconIndices = [];

    for (const key of this._activeIconPackIds.toReversed()) {
      const pack = this._availableIconPacks.find((p) => p.id === key);
      if (!pack) continue;

      this.activeIconPacks.push(pack);

      const index: IconPackLookupIndex = {
        uniqueByUmid: new Map(),
        uniqueByPath: new Map(),
        uniqueByFilename: new Map(),
        sharedByExtension: new Map(),
        customByKey: new Map(),
      };

      // Keep the first entry for each key: entries are ordered by preference inside
      // a pack, just as Array.find() was before this index was introduced.
      for (const entry of pack.entries) {
        switch (entry.type) {
          case "unique": {
            if (entry.umid && !index.uniqueByUmid.has(entry.umid)) {
              index.uniqueByUmid.set(entry.umid, entry);
            }

            if (entry.path) {
              if (!index.uniqueByPath.has(entry.path)) {
                index.uniqueByPath.set(entry.path, entry);
              }

              const filename = entry.path.split("\\").pop();
              if (filename && !index.uniqueByFilename.has(filename)) {
                index.uniqueByFilename.set(filename, entry);
              }
            }
            break;
          }
          case "shared":
            if (!index.sharedByExtension.has(entry.extension)) {
              index.sharedByExtension.set(entry.extension, entry);
            }
            break;
          case "custom":
            if (!index.customByKey.has(entry.key)) {
              index.customByKey.set(entry.key, entry);
            }
            break;
        }
      }

      this.activeIconIndices.push(index);
    }
  }

  /**
   * Creates an instance of IconPackManager. This intance will be updated when
   * the list of icon packs or the settings changes, so just having one global instance is enough.
   *
   * @returns A new instance of IconPackManager
   */
  public static async create(): Promise<IconPackManager> {
    const instance = new IconPackManager(
      (await IconPackList.getAsync()).asArray(),
      (await Settings.getAsync()).inner.activeIconPacks,
    );
    instance.resolveAvailableIcons();
    instance.cacheActiveIconPacks();
    return instance;
  }

  /**
   * Registers a callback to be invoked when the list of active icon packs changes.
   * This method also sets up listeners to detect changes in the icon pack list and
   * the active icon packs settings. If no callbacks are registered beforehand, the
   * listeners are initialized. When no callbacks remain registered, the listeners are stopped.
   *
   * @param {() => void} cb - The callback to be invoked when the list of active icon packs changes.
   *                          This callback takes no arguments and returns no value.
   * @returns {Promise<UnlistenFn>} A promise that resolves to an `UnlistenFn` function. When invoked,
   *                                this function unregisters the callback and stops listening for changes
   *                                if no other callbacks are registered.
   *
   * @example
   * const manager = await IconPackManager.create();
   * const unlisten = await manager.onChange(() => {
   *   console.log("Icon packs changed: ", manager.actives);
   * });
   *
   * // Later, to stop listening for changes:
   * unlisten();
   *
   * @remarks
   * - The `this` context inside the callback refers to the `IconPackManager` instance, provided the callback
   *   is not rebound to another context (e.g., using `bind`, `call`, or `apply`).
   * - If the callback is defined as an arrow function, `this` will be lexically bound to the surrounding context.
   * - If the callback is a regular function, ensure it is bound correctly to avoid `this` being `undefined` (in strict mode)
   *   or the global object (in non-strict mode).
   *
   * @see {@link IconPackManager} for the class this method belongs to.
   * @see {@link UnlistenFn} for the type of the function returned by this method.
   */
  public async onChange(cb: () => void): Promise<UnlistenFn> {
    this.callbacks.add(cb);

    if (!this.isListeningForChanges) {
      this.isListeningForChanges = true;
      const unlistenerIcons = await IconPackList.onChange((list) => {
        this._availableIconPacks = JSON.parse(JSON.stringify(list.all()));
        this.resolveAvailableIcons();
        this.cacheActiveIconPacks();
        this.callbacks.forEach((cb) => cb());
      });
      const unlistenerSettings = await Settings.onChange((settings) => {
        this._activeIconPackIds = settings.inner.activeIconPacks;
        this.cacheActiveIconPacks();
        this.callbacks.forEach((cb) => cb());
      });
      this.unlisteners = [unlistenerIcons, unlistenerSettings];
    }

    return () => {
      this.callbacks.delete(cb);
      if (this.callbacks.size === 0) {
        this.unlisteners.forEach((unlisten) => unlisten());
        this.unlisteners = [];
        this.isListeningForChanges = false;
      }
    };
  }

  public getIconEntry(args: GetIconArgs): IconPackEntry | null {
    const {
      path,
      umid,
      __seen = new Set<string>(),
    } = args as GetIconArgs & { __seen?: Set<string> };

    // If neither path nor UMID is provided, return null
    if (!path && !umid) {
      return null;
    }

    const lowerPath = path?.toLowerCase();

    // Extract extension only when there is an actual dot after the last path separator.
    const lastDot = lowerPath?.lastIndexOf(".");
    const lastSlash = lowerPath?.lastIndexOf("\\") ?? -1;
    const extension = lastDot !== undefined && lastDot > lastSlash
      ? lowerPath!.slice(lastDot + 1)
      : undefined;

    // Add the starting path to __seen so direct A→B→A cycles are caught in 2 hops.
    if (lowerPath) {
      __seen.add(lowerPath);
    }

    // Preserve pack priority exactly: for each pack, UMID is checked first, then exact
    // path, then executable filename. A higher-priority pack's path entry still wins over
    // a lower-priority pack's UMID entry, matching the previous ordered Array.find loops.
    for (const index of this.activeIconIndices) {
      let entry: UniqueIconEntry | undefined;

      if (umid) {
        entry = index.uniqueByUmid.get(umid);
      }

      if (!entry && lowerPath) {
        entry = index.uniqueByPath.get(lowerPath);

        if (!entry && extension === "exe") {
          const filename = lowerPath.split("\\").pop();
          if (filename) {
            entry = index.uniqueByFilename.get(filename);
          }
        }
      }

      if (entry) {
        if (entry.redirect) {
          // break circular references
          if (__seen.has(entry.redirect)) {
            return null;
          }
          __seen.add(entry.redirect);
          return this.getIconEntry({ path: entry.redirect, __seen } as GetIconArgs);
        }

        if (entry.icon) {
          return entry;
        }
      }
    }

    // Search by file extension after all unique matches have been considered.
    if (!extension) {
      return null;
    }

    for (const index of this.activeIconIndices) {
      const entry = index.sharedByExtension.get(extension);
      if (entry) {
        return entry;
      }
    }

    return null;
  }

  public getIconPath(args: GetIconArgs): IIcon | null {
    const entry = this.getIconEntry(args);
    return entry?.icon || null;
  }

  public getIcon({ path, umid }: GetIconArgs): IIcon | null {
    const iconPath = this.getIconPath({ path, umid });
    return iconPath ? resolveAsSrc(iconPath) : null;
  }

  /**
   * Will return the special missing icon path from the highest priority icon pack.
   * If no icon pack haves a missing icon, will return null.
   */
  public getMissingIconPath(): IIcon | null {
    for (const pack of this.activeIconPacks) {
      if (pack.missing) {
        return pack.missing;
      }
    }
    return null;
  }

  /**
   * Will return the special missing icon SRC from the highest priority icon pack.
   * If no icon pack haves a missing icon, will return null.
   */
  public getMissingIcon(): IIcon | null {
    const iconPath = this.getMissingIconPath();
    return iconPath ? resolveAsSrc(iconPath) : null;
  }

  /**
   * Will return the specific icon entry from the highest priority icon pack.
   * If no icon pack haves the searched icon, will return null.
   */
  public getCustomIconEntry(name: string): IconPackEntry | null {
    for (const index of this.activeIconIndices) {
      const entry = index.customByKey.get(name);
      if (entry) {
        return entry;
      }
    }
    return null;
  }

  /**
   * Will return the specific icon path from the highest priority icon pack.
   * If no icon pack haves the searched icon, will return null.
   */
  public getCustomIconPath(name: string): IIcon | null {
    const entry = this.getCustomIconEntry(name);
    return entry?.icon || null;
  }

  /**
   * Will return the specific icon SRC from the highest priority icon pack.
   * If no icon pack haves the searched icon, will return null.
   */
  public getCustomIcon(name: string): IIcon | null {
    const iconPath = this.getCustomIconPath(name);
    return iconPath ? resolveAsSrc(iconPath) : null;
  }

  /**
   * This method doesn't take in care icon packs, just extracts the inherited icon into system's icon pack
   * if it's not already there.
   *
   * @param filePath The path to the app could be umid o full path
   * @example
   * const iconPath = instance.extractIcon({
   *   path: "C:\\Program Files\\Steam\\steam.exe"
   * });
   * const iconPath = instance.extractIcon({
   *   umid: "Seelen.SeelenUI_p6yyn03m1894e!App"
   * });
   */
  public static requestIconExtraction(obj: GetIconArgs): Promise<void> {
    return invoke(SeelenCommand.GetIcon, obj);
  }

  /**
   * This will delete all stored icons on the system icon pack.\
   * All icons should be regenerated after calling this method.
   */
  public static clearCachedIcons(): Promise<void> {
    return invoke(SeelenCommand.StateDeleteCachedIcons);
  }
}

function resolveIcon(parent: string, icon: IIcon): IIcon {
  return {
    base: icon.base ? `${parent}\\${icon.base}` : null,
    light: icon.light ? `${parent}\\${icon.light}` : null,
    dark: icon.dark ? `${parent}\\${icon.dark}` : null,
    mask: icon.mask ? `${parent}\\${icon.mask}` : null,
    isAproximatelySquare: icon.isAproximatelySquare,
  };
}

function resolveAsSrc(icon: IIcon): IIcon {
  return {
    base: icon.base ? convertFileSrc(icon.base) : null,
    light: icon.light ? convertFileSrc(icon.light) : null,
    dark: icon.dark ? convertFileSrc(icon.dark) : null,
    mask: icon.mask ? convertFileSrc(icon.mask) : null,
    isAproximatelySquare: icon.isAproximatelySquare,
  };
}

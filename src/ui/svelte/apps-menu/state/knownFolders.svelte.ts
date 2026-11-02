import { invoke, SeelenCommand, SeelenEvent, subscribe } from "@seelen-ui/lib";
import { FolderType, type StartMenuItem } from "@seelen-ui/lib/types";
import { lazyRune } from "libs/ui/svelte/utils";

const desktop = lazyRune(() => invoke(SeelenCommand.GetUserFolderContent, { folderType: FolderType.Desktop }));
const downloads = lazyRune(() => invoke(SeelenCommand.GetUserFolderContent, { folderType: FolderType.Downloads }));
const documents = lazyRune(() => invoke(SeelenCommand.GetUserFolderContent, { folderType: FolderType.Documents }));
const music = lazyRune(() => invoke(SeelenCommand.GetUserFolderContent, { folderType: FolderType.Music }));
const pictures = lazyRune(() => invoke(SeelenCommand.GetUserFolderContent, { folderType: FolderType.Pictures }));
const videos = lazyRune(() => invoke(SeelenCommand.GetUserFolderContent, { folderType: FolderType.Videos }));

subscribe(SeelenEvent.UserFolderChanged, ({ payload: { ofFolder, content } }) => {
  switch (ofFolder) {
    case FolderType.Desktop:
      desktop.value = content;
      break;
    case FolderType.Downloads:
      downloads.value = content;
      break;
    case FolderType.Documents:
      documents.value = content;
      break;
    case FolderType.Music:
      music.value = content;
      break;
    case FolderType.Pictures:
      pictures.value = content;
      break;
    case FolderType.Videos:
      videos.value = content;
      break;
  }
});

let loadPromise: Promise<void> | undefined;

/**
 * Folder enumeration is only required for file search. Do not fetch six user-folder
 * listings while the Start-menu popup is mounting.
 */
export function ensureKnownFoldersLoaded(): Promise<void> {
  if (!loadPromise) {
    loadPromise = Promise.all([
      desktop.init(),
      downloads.init(),
      documents.init(),
      music.init(),
      pictures.init(),
      videos.init(),
    ])
      .then(() => undefined)
      .catch((error) => {
        loadPromise = undefined;
        console.error("Failed to load known-folder contents:", error);
      });
  }
  return loadPromise;
}

function pathAsItem(path: string): StartMenuItem {
  return {
    path,
    umid: null,
    display_name: path.split(/[\\/]/g).pop() || "",
    target: null,
    toast_activator: null,
  };
}

const _foldersAsStartMenuItems = $derived.by(() => {
  // LazyRune initialization is reactive, so the derived list fills in once the
  // requested folder results arrive without blocking the menu or throwing early.
  return [
    desktop.isInitialized() ? desktop.value.map(pathAsItem) : [],
    downloads.isInitialized() ? downloads.value.map(pathAsItem) : [],
    documents.isInitialized() ? documents.value.map(pathAsItem) : [],
    music.isInitialized() ? music.value.map(pathAsItem) : [],
    pictures.isInitialized() ? pictures.value.map(pathAsItem) : [],
    videos.isInitialized() ? videos.value.map(pathAsItem) : [],
  ].flat();
});

export const foldersAsStartMenuItems = {
  get value() {
    return _foldersAsStartMenuItems;
  },
};


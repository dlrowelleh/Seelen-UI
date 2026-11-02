import { invoke, SeelenCommand, SeelenEvent, subscribe, Widget } from "@seelen-ui/lib";
import { SeelenWegSide, type UserAppWindow } from "@seelen-ui/lib/types";
import { lazyRune } from "libs/ui/svelte/utils";

const interactables = lazyRune<UserAppWindow[]>(() => invoke(SeelenCommand.GetUserAppWindows));
subscribe(SeelenEvent.UserAppWindowsChanged, interactables.setByPayload);

const previews = lazyRune<Record<number, { data: string }>>(() => invoke(SeelenCommand.GetUserAppWindowsPreviews));
subscribe(SeelenEvent.UserAppWindowsPreviewsChanged, previews.setByPayload);

await interactables.init();

let hwnds = $state<number[]>([]);
let position = $state<SeelenWegSide>(SeelenWegSide.Bottom);

Widget.self.onTrigger(({ customArgs }) => {
  hwnds = (customArgs?.hwnds as number[]) ?? [];
  position = (customArgs?.position as SeelenWegSide) ?? SeelenWegSide.Bottom;

  // The preview widget is lazy-created on its first opening. Initialize once there,
  // then explicitly refresh the cache on subsequent openings.
  const refresh = previews.isInitialized()
    ? invoke(SeelenCommand.GetUserAppWindowsPreviews).then((snapshot) => {
        previews.value = snapshot;
      })
    : previews.init();

  void refresh.catch((error) => {
    console.error("Failed to refresh dock preview thumbnails:", error);
  });
});

const currentInteractables = $derived(interactables.value?.filter((w) => hwnds.includes(w.hwnd)) ?? []);

class PreviewState {
  get currentInteractables() {
    return currentInteractables;
  }

  get previews() {
    return previews;
  }

  get position() {
    return position;
  }
}

export const previewState = new PreviewState();

import { getRootContainer } from "libs/ui/react/utils/index.ts";
import { mount } from "svelte";
import App from "./App.svelte";
import { Widget, SeelenEvent, subscribe } from "@seelen-ui/lib";
import { onTriggered } from "./state/positioning.svelte.ts";

import "@seelen-ui/lib/styles/reset.css";

const widget = Widget.getCurrent();

await widget.init({ hideOnFocusLoss: true });
await widget.window.setFocusable(true);

// The menu starts hidden (the widget is marked ready with show:false). Track visibility
// from the existing global-focus stream instead of making a synchronous native IPC query
// on every Win-key press.
let menuVisible = false;
subscribe(SeelenEvent.GlobalFocusChanged, ({ payload }) => {
  if (payload.hwnd !== widget.windowId && payload.ownerHwnd !== widget.windowId) {
    menuVisible = false;
  }
});

widget.onTrigger((args) => {
  if (menuVisible) {
    menuVisible = false;
    void widget.hide();
    return;
  }

  menuVisible = true;
  void onTriggered(args.desiredPosition).catch((error) => {
    menuVisible = false;
    console.error("Failed to show apps menu:", error);
  });
});

mount(App, {
  target: getRootContainer(),
});

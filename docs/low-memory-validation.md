# Seelen UI low-memory validation

This workflow measures the effect of the low-memory WebView2 profile. It does
  not assume that a smaller working set means lower total committed memory;
  compare both **Working Set** and **Private Memory**.

## Build

The `Low-memory Windows setup build` workflow builds the x64 NSIS setup
  installer and uploads it as `seelen-ui-low-memory-setup-x64`.

## Repeatable idle measurement

1. Reboot Windows, sign in, and wait two minutes.
2. Close unrelated Edge windows and applications that embed WebView2.
3. Install and launch the same Seelen UI build for each run.
4. Keep the same widgets enabled, same monitor layout, same
   hardware-acceleration setting, and same power mode.
5. Wait two minutes after startup.
6. Run from the repository root:

   ```powershell
   powershell -ExecutionPolicy Bypass -File .\\scripts\\measure-low-memory.ps1 -DurationSeconds 180 -SampleIntervalSeconds 2 -Output baseline.csv
   ```

7. Repeat after the desktop has been idle for at least one minute. Save a
   separate CSV.
8. Compare average and peak Working Set and Private Memory by process. Sum the
   rows for `seelen-ui`, `slu`, `slu-service`, and the Seelen-owned
   `msedgewebview2` processes. Exclude unrelated WebView2 processes.

## Scenarios to test

- Cold start, after two minutes.
- Desktop idle, all normal widgets visible.
- A popup/settings widget opened and then closed.
- Game mode active.
- Third-party widget hidden but intentionally running background work.
- Restore after sleep / interactive-session resume.

Record startup-to-interactive latency and whether any widget loses state or
  stops background work.

## GPU measurement

Use Task Manager > Details and add the **GPU** and **GPU engine** columns.
  Record the Seelen UI and WebView2 process GPU engine usage under the same
  scenarios. GPU engine percentages are not additive across different engines
  and should be reported separately.

## Interpretation

- WebView2 `TrySuspend` is best-effort. A renderer may decline suspension.
- The low memory target is also best-effort and may page inactive memory to
  disk. A widget may briefly respond more slowly after becoming active.
- Internal widgets are eligible for suspension because they are owned by Seelen.
- Third-party widgets are not suspended: they receive the low-memory target so
  timers, audio, and network activity can continue.
- Do not compare a run with hardware acceleration enabled to one with it
  disabled and attribute all differences to WebView lifecycle changes.
- Do not use a single Task Manager number as the total: WebView2 splits work
  across browser, renderer, GPU, and utility processes.

## Known scope

This change keeps WebView2 and Tauri. It does not claim zero-memory operation:
  visible widgets necessarily retain a renderer and GPU/window resources.
  Destruction/recreation of hidden widget windows is not enabled because the
  current widget deployment lifecycle automatically reconciles destroyed
  windows; implementing eviction safely requires a persistent pod state and
  restoration contract.

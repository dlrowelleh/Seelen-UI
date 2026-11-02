use std::path::PathBuf;

use base64::Engine;
use seelen_core::{
    resource::WidgetId,
    state::{Widget, WidgetLoader, WidgetPreset},
    system_state::MonitorId,
};

use tauri::Manager;
use windows::Win32::Foundation::HWND;
use windows_core::BOOL;

use crate::{
    app::get_app_handle,
    error::{Result, ResultLogExt},
    state::application::FULL_STATE,
    utils::constants::SEELEN_COMMON,
    windows_api::WindowsApi,
};

pub struct WidgetWebview(pub tauri::WebviewWindow);

impl WidgetWebview {
    pub fn create(
        widget: &Widget,
        label: &WidgetWebviewLabel,
        owner_hwnd: Option<isize>,
    ) -> Result<Self> {
        let state = FULL_STATE.load();
        let title = widget.metadata.display_name.get(state.locale());

        let args = WebviewArgs::create(
            state.settings.hardware_acceleration || widget.force_hardware_acceleration,
            state.settings.unstable_optimizations,
        );

        let url = match widget.loader {
            WidgetLoader::Legacy => {
                return Err("Legacy widgets are not supported by the new widget loader".into());
            }
            WidgetLoader::InternalReact => {
                let resource_name = widget
                    .id
                    .resource_name()
                    .ok_or("Can't get internal resource path")?;
                tauri::WebviewUrl::App(format!("react/{resource_name}/index.html").into())
            }
            WidgetLoader::Internal => {
                let resource_name = widget
                    .id
                    .resource_name()
                    .ok_or("Can't get internal resource path")?;
                tauri::WebviewUrl::App(format!("svelte/{resource_name}/index.html").into())
            }
            WidgetLoader::ThirdParty => {
                tauri::WebviewUrl::App("vanilla/third_party/index.html".into())
            }
        };

        let mut builder = tauri::WebviewWindowBuilder::new(get_app_handle(), &label.raw, url)
            .title(title)
            .transparent(true)
            .visible(false);

        if matches!(
            widget.preset,
            WidgetPreset::Desktop | WidgetPreset::Overlay | WidgetPreset::Popup
        ) {
            builder = builder
                .decorations(false)
                .shadow(false)
                .skip_taskbar(true)
                .minimizable(false)
                .maximizable(false)
                .closable(false);
        }

        if matches!(widget.preset, WidgetPreset::Desktop | WidgetPreset::Overlay) {
            builder = builder.focusable(false).focused(false);
        }

        match widget.preset {
            WidgetPreset::Desktop => {
                builder = builder.always_on_bottom(true);
            }
            WidgetPreset::Overlay | WidgetPreset::Popup => {
                // the window manager overlay is not topmost, its z-order is handled by the wm hook
                let always_on_top = widget.id != WidgetId::known_wm();
                builder = builder.always_on_top(always_on_top).resizable(false);
            }
            _ => {}
        }

        if let Some(owner) = owner_hwnd {
            // SAFETY: HWND in windows 0.61 (tauri) and 0.62 (ours) share the same memory layout
            #[allow(clippy::missing_transmute_annotations)]
            {
                builder = builder.owner_raw(unsafe { std::mem::transmute(owner) });
            }
        }

        let window = builder
            // wry silently denies new windows if no handler is set, so `target="_blank"` links
            // and `window.open` did nothing (tauri-plugin-shell used to intercept these clicks).
            // External links are opened on the default handler instead.
            .on_new_window(|url, _features| {
                if matches!(url.scheme(), "http" | "https" | "mailto" | "tel") {
                    WindowsApi::execute(url.to_string(), None, None, false).log_error();
                }
                tauri::webview::NewWindowResponse::Deny
            })
            .data_directory(args.data_directory())
            .additional_browser_args(&args.to_string())
            .build()?;

        // Widgets handle their own show/hide animations, avoid the ones from the system.
        // SAFETY: HWND in windows 0.61 (tauri) and 0.62 (ours) share the same memory layout
        WindowsApi::set_system_transitions_disabled(HWND(window.hwnd()?.0), true).log_error();
        Ok(Self(window))
    }

    pub fn reload(&self) {
        self.0.reload().log_error();
    }

    /// Handle to the underlying webview, cheap to clone, safe to hold outside
    /// this struct's lifetime (unlike `WidgetWebview` itself, which destroys the
    /// native window on drop).
    pub fn handle(&self) -> tauri::WebviewWindow {
        self.0.clone()
    }
}

/// Makes the webview render 1 CSS px = 1 physical px (`devicePixelRatio` = 1) on every monitor.
///
/// By default WebView2 tracks the monitor DPI itself (`ShouldDetectMonitorScaleChanges`) and
/// updates its `RasterizationScale` asynchronously after the window crosses monitors, so any
/// zoom-based compensation done from the webview races against it (and against `WM_DPICHANGED`
/// not being delivered to hidden windows). Taking ownership of the scale and pinning it to 1.0
/// removes the race entirely: the scale never changes again, whatever monitor the window is on.
pub fn lock_rasterization_scale_to_one(webview: &tauri::WebviewWindow) -> Result<()> {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller3;
    use windows_core::Interface;

    webview.with_webview(|platform_webview| {
        let controller = platform_webview.controller();
        let result = (|| unsafe {
            // reset any zoom compensation applied before (zoom persists across reloads)
            controller.SetZoomFactor(1.0)?;
            let controller = controller.cast::<ICoreWebView2Controller3>()?;
            controller.SetShouldDetectMonitorScaleChanges(false)?;
            controller.SetRasterizationScale(1.0)
        })();

        if let Err(err) = result {
            log::error!("Failed to lock webview rasterization scale: {err:?}");
        }
    })?;
    Ok(())
}

/// Suspends an invisible WebView2 renderer after it has been idle.
///
/// WebView2 pauses JavaScript timers and animations and can release renderer memory.
/// This is best-effort: pages with active work may decline suspension. The controller
/// must be invisible before TrySuspend is called.
pub fn suspend_if_hidden(webview: &tauri::WebviewWindow) {
    use webview2_com::{
        Microsoft::Web::WebView2::Win32::ICoreWebView2_3, TrySuspendCompletedHandler,
    };
    use windows_core::Interface;

    webview
        .with_webview(|platform_webview| {
            let controller = platform_webview.controller();
            let mut is_visible = BOOL(1);
            if let Err(err) = unsafe { controller.IsVisible(&mut is_visible) } {
                log::debug!("Failed to read WebView2 visibility before suspension: {err}");
                return;
            }
            if is_visible.as_bool() {
                return;
            }

            let core_webview = match unsafe { controller.CoreWebView2() } {
                Ok(webview) => webview,
                Err(err) => {
                    log::debug!("Failed to get CoreWebView2 for suspension: {err}");
                    return;
                }
            };

            let webview = match core_webview.cast::<ICoreWebView2_3>() {
                Ok(webview) => webview,
                Err(err) => {
                    // Older WebView2 runtimes may not expose ICoreWebView2_3.
                    log::debug!("WebView2 runtime does not support TrySuspend: {err}");
                    return;
                }
            };

            let mut is_suspended = BOOL(0);
            if let Err(err) = unsafe { webview.IsSuspended(&mut is_suspended) } {
                log::debug!("Failed to query WebView2 suspension state: {err}");
                return;
            }
            if is_suspended.as_bool() {
                return;
            }

            let handler =
                TrySuspendCompletedHandler::create(Box::new(move |error_code, is_successful| {
                    match error_code {
                        Ok(()) if is_successful => {
                            log::trace!("Hidden WebView2 renderer suspended");
                        }
                        Ok(()) => {
                            // WebView2 can decline suspension when the page has active work.
                            log::debug!("WebView2 declined suspension; it will be retried later");
                        }
                        Err(err) => {
                            log::debug!("WebView2 TrySuspend callback failed: {err}");
                        }
                    }
                    Ok(())
                }));

            if let Err(err) = unsafe { webview.TrySuspend(&handler) } {
                log::debug!("Failed to request WebView2 suspension: {err}");
            }
        })
        .log_error();
}

/// Resume a suspended WebView2 before dispatching an event to its JavaScript context.
/// WebView2 also resumes automatically when made visible, but explicit resumption is
/// needed for events dispatched while the widget is still hidden.
pub fn resume_if_suspended(webview: &tauri::WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_3;
    use windows_core::Interface;

    webview
        .with_webview(|platform_webview| {
            let controller = platform_webview.controller();
            let core_webview = match unsafe { controller.CoreWebView2() } {
                Ok(webview) => webview,
                Err(err) => {
                    log::debug!("Failed to get CoreWebView2 before resume: {err}");
                    return;
                }
            };

            let webview = match core_webview.cast::<ICoreWebView2_3>() {
                Ok(webview) => webview,
                Err(_) => return,
            };

            let mut is_suspended = BOOL(0);
            if let Err(err) = unsafe { webview.IsSuspended(&mut is_suspended) } {
                log::debug!("Failed to query WebView2 suspension state before resume: {err}");
                return;
            }
            if is_suspended.as_bool() {
                if let Err(err) = unsafe { webview.Resume() } {
                    log::warn!("Failed to resume suspended WebView2: {err}");
                } else {
                    log::trace!("Resumed suspended WebView2 renderer");
                }
            }
        })
        .log_error();
}

impl Drop for WidgetWebview {
    fn drop(&mut self) {
        // Only destroy if Tauri's manager still holds the window. When
        // WindowEvent::Destroyed fires, Tauri has already removed it from the
        // manager (get_webview_window returns None), so calling destroy() again
        // would cause re-entrant ZwUserDestroyWindow → FATAL_USER_CALLBACK_EXCEPTION.
        let label = self.0.label().to_owned();
        if get_app_handle().get_webview_window(&label).is_some() {
            let _ = self.0.destroy();
        }
    }
}

// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WidgetWebviewLabel {
    /// this should be used as the real webview label
    pub raw: String,
    /// this is the decoded label, useful for debugging and logging
    pub decoded: String,
    /// widget id from this label was created
    pub widget_id: WidgetId,
    pub monitor_id: Option<MonitorId>,
    pub instance_id: Option<uuid::Uuid>,
}

impl WidgetWebviewLabel {
    pub fn new(
        widget_id: &WidgetId,
        monitor_id: Option<&str>,
        instance_id: Option<&uuid::Uuid>,
    ) -> Self {
        let mut label = widget_id.to_string();
        let with_monitor_id = monitor_id.is_some();
        let with_instance_id = instance_id.is_some();
        if with_monitor_id || with_instance_id {
            label.push('?');
        }

        if let Some(monitor_id) = monitor_id {
            label.push_str(&format!("monitorId={}", urlencoding::encode(monitor_id)));
        }

        if let Some(instance_id) = instance_id {
            if with_monitor_id {
                label.push('&');
            }
            label.push_str(&format!(
                "instanceId={}",
                urlencoding::encode(&instance_id.to_string())
            ));
        }

        Self {
            raw: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&label),
            decoded: label,
            widget_id: widget_id.clone(),
            monitor_id: monitor_id.map(MonitorId::from),
            instance_id: instance_id.cloned(),
        }
    }

    pub fn try_from_raw(raw: &str) -> Result<Self> {
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(raw)?;
        let decoded = String::from_utf8(decoded)?;

        let mut parts = decoded.splitn(2, '?');
        let widget_id = WidgetId::from(parts.next().expect("Invalid label"));

        let mut monitor_id = None;
        let mut instance_id = None;
        if let Some(query) = parts.next() {
            for param in query.split('&') {
                if let Some(value) = param.strip_prefix("monitorId=") {
                    let decoded_value = urlencoding::decode(value).unwrap_or_default();
                    monitor_id = Some(MonitorId::from(decoded_value.as_ref()));
                } else if let Some(value) = param.strip_prefix("instanceId=") {
                    let decoded_value = urlencoding::decode(value).unwrap_or_default();
                    instance_id = decoded_value.parse::<uuid::Uuid>().ok();
                }
            }
        }

        Ok(Self {
            raw: raw.to_string(),
            decoded,
            widget_id,
            monitor_id,
            instance_id,
        })
    }
}

impl std::fmt::Display for WidgetWebviewLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.decoded)
    }
}

// =============================================================================

pub struct WebviewArgs {
    args: Vec<String>,
    with_gpu: bool,
    unstable: bool,
}

impl WebviewArgs {
    const BASE_ARGS: &[&str] = &[
        "--disable-features=translate,msWebOOUI,msPdfOOUI,msSmartScreenProtection,RendererAppContainer,BackForwardCache,InterestCohort,SharedArrayBuffer,OptimizationHints,AutofillServerCommunication,PaintHolding",
        "--no-first-run",
        "--disable-site-isolation-trials",
        "--disk-cache-size=0",
        "--disable-application-cache",
        "--media-cache-size=0",
        "--disable-extensions",
        "--disable-component-extensions-with-background-pages",
        "--disable-ipc-flooding-protection",
        "--disable-breakpad",
        "--disable-crash-reporter",
        "--disable-background-networking",
        "--disable-component-update",
        // Do not disable renderer backgrounding: Chromium/WebView2 can deprioritize hidden
        // widgets, reducing timer wakeups and idle CPU use.
        "--disable-sync",
        "--no-pings",
        // maybe causes more resources than it reduces
        // "--aggressive-cache-discard",
    ];

    const GPU_ARGS: &[&str] = &[
        "--enable-gpu",
        "--enable-accelerated-video-decode",
        "--enable-gpu-rasterization",
        "--enable-zero-copy",
        "--enable-native-gpu-memory-buffers",
        "--enable-oop-rasterization",
        "--use-angle=d3d11", // Media Foundation + DXVA + D3D11 in windows is the most optimized
    ];

    const PERFORMANCE_ARGS: &[&str] = &[
        // unstable flag that causes more issues than it solves
        // "--enable-low-end-device-mode",
        // this completely removes the gpu process
        "--in-process-gpu",
        "--disable-gpu",
        "--disable-gpu-compositing",
        "--disable-gpu-shader-disk-cache",
        "--disable-accelerated-video-encode",
        "--disable-gpu-rasterization",
        "--disable-software-rasterizer",
    ];

    // Share Chromium renderer processes between WebViews belonging to the same
    // site. This materially reduces the per-widget process overhead. The trade-off
    // is a larger crash domain: a renderer crash can affect multiple same-site widgets.
    // This is enabled by default because Seelen's internal widgets are shipped as a
    // small number of app origins and memory usage is a primary constraint.
    const PROCESS_SHARING_ARGS: &[&str] = &["--process-per-site"];

    pub fn create(with_gpu: bool, unstable_optimizations: bool) -> Self {
        let mut args: Vec<String> = Self::BASE_ARGS.iter().map(|s| s.to_string()).collect();

        if with_gpu {
            args.extend(Self::GPU_ARGS.iter().map(|s| s.to_string()));
        } else {
            args.extend(Self::PERFORMANCE_ARGS.iter().map(|s| s.to_string()));
        };

        // Unlike the former opt-in unstable flag, renderer sharing is now part
        // of the default low-memory profile.
        args.extend(Self::PROCESS_SHARING_ARGS.iter().map(|s| s.to_string()));

        Self {
            args,
            with_gpu,
            unstable: unstable_optimizations,
        }
    }

    pub fn data_directory(&self) -> PathBuf {
        let foldername = match (self.with_gpu, self.unstable) {
            (true, true) => "gpu-unstable",
            (true, false) => "gpu",
            (false, true) => "no-gpu-unstable",
            (false, false) => "no-gpu",
        };

        SEELEN_COMMON.app_cache_dir().join(foldername)
    }
}

impl std::fmt::Display for WebviewArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.args.join(" "))
    }
}

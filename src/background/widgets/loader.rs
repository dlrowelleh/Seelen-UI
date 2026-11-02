use std::{
    sync::{Arc, atomic::AtomicU8},
    time::{Duration, Instant},
};

use seelen_core::state::{Widget, WidgetInstanceMode, WidgetLoader, WidgetStatus};
use tauri::{Emitter, Listener};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use uuid::Uuid;

use crate::{
    app::get_app_handle,
    get_tokio_handle,
    modules::monitors::MonitorManager,
    resources::RESOURCES,
    state::application::FULL_STATE,
    utils::lock_free::SyncHashMap,
    widgets::{
        WidgetWebviewLabel,
        manager::{GAME_MODE_ACTIVE, WIDGET_MANAGER},
        notify_widget_statuses_change,
        webview::{self, WidgetWebview},
    },
    windows_api::event_window::IS_INTERACTIVE_SESSION,
};

pub enum PodSource {
    Static,
    Runtime,
}

// Background widgets are mostly static; avoid waking every renderer every few seconds.
const LIVENESS_PROVE_INTERVAL: Duration = Duration::from_secs(60);
const LIVENESS_PROVE_WAIT_TIMEOUT: Duration = Duration::from_secs(3);
const LIVENESS_PROVE_MAX_RETRIES: u8 = 5;
// Grace period after session resume or soft_restart to let the webview finish reloading.
const LIVENESS_RELOAD_GRACE_PERIOD: Duration = Duration::from_secs(10);
// Hidden internal widgets are suspended after a short grace period. Third-party widgets
// retain script execution and receive WebView2's low-memory target after the same grace period.
const HIDDEN_WEBVIEW_SUSPEND_CHECK_INTERVAL: Duration = Duration::from_secs(15);
const HIDDEN_WEBVIEW_SUSPEND_GRACE_PERIOD: Duration = Duration::from_secs(20);

pub struct WidgetDeployment {
    pub definition: Arc<Widget>,
    pub pods: SyncHashMap<WidgetWebviewLabel, WidgetPod>,
}

impl WidgetDeployment {
    pub fn new(definition: Arc<Widget>) -> Self {
        log::trace!("Registering widget: {}", definition.id);
        Self {
            definition,
            pods: SyncHashMap::new(),
        }
    }

    /// Will revaluate all widget instances and remove or add them based on current user settings
    pub fn reconcile(&self) {
        match self.definition.instances {
            WidgetInstanceMode::Single => {
                if self.pods.is_empty() {
                    let label = WidgetWebviewLabel::new(&self.definition.id, None, None);
                    let instance = WidgetPod::create(label, None, PodSource::Static);
                    self.pods.upsert(instance.label.clone(), instance);
                }
            }
            WidgetInstanceMode::Multiple => {
                let nil_id = Uuid::nil();
                if !self.definition.lazy && self.pods.is_empty() {
                    let label = WidgetWebviewLabel::new(&self.definition.id, None, Some(&nil_id));
                    let instance = WidgetPod::create(label, None, PodSource::Static);
                    self.pods.upsert(instance.label.clone(), instance);
                }

                let replicas_ids = FULL_STATE
                    .load()
                    .get_widget_instances_ids(&self.definition.id);

                // Remove deleted static instances; runtime pods are never evicted by reconcile.
                self.pods.retain(|(label, pod)| {
                    if matches!(pod.source, PodSource::Runtime) {
                        return true;
                    }
                    let instance_id = label.instance_id.expect("Missing instance id");
                    instance_id == nil_id || replicas_ids.contains(&instance_id)
                });

                // Add new instances
                for replica_id in replicas_ids {
                    if !self
                        .pods
                        .any(|(label, _)| label.instance_id == Some(replica_id))
                    {
                        let label =
                            WidgetWebviewLabel::new(&self.definition.id, None, Some(&replica_id));
                        let instance = WidgetPod::create(label, None, PodSource::Static);
                        self.pods.upsert(instance.label.clone(), instance);
                    }
                }
            }
            WidgetInstanceMode::ReplicaByMonitor => {
                let configs = FULL_STATE.load();
                let connected_ids = MonitorManager::instance().get_cached_ids();

                // Remove disabled or disconnected instances
                self.pods.retain(|(label, _)| {
                    let monitor_id = label.monitor_id.as_ref().expect("Missing monitor id");
                    connected_ids.contains(monitor_id)
                        && configs.is_widget_enable_on_monitor(&self.definition.id, monitor_id)
                });

                // Add new/enabled instances
                for monitor_id in connected_ids {
                    if self
                        .pods
                        .any(|(label, _)| label.monitor_id.as_ref() == Some(&monitor_id))
                    {
                        continue;
                    }

                    if !configs.is_widget_enable_on_monitor(&self.definition.id, &monitor_id) {
                        continue;
                    }

                    let label =
                        WidgetWebviewLabel::new(&self.definition.id, Some(&monitor_id), None);
                    let instance = WidgetPod::create(label, None, PodSource::Static);
                    self.pods.upsert(instance.label.clone(), instance);
                }
            }
        }
    }

    pub fn start_all_webviews(&self) {
        self.pods.for_each(|(_k, pod)| {
            pod.run(&self.definition);
        });
    }

    pub fn start_webview(&self, label: &WidgetWebviewLabel) {
        self.pods.get(label, |pod| {
            pod.run(&self.definition);
        });
    }

    pub fn create_runtime_instance(&self, instance_id: &Uuid, owner_hwnd: Option<isize>) {
        let label = WidgetWebviewLabel::new(&self.definition.id, None, Some(instance_id));
        let instance = WidgetPod::create(label, owner_hwnd, PodSource::Runtime);
        self.pods.upsert(instance.label.clone(), instance);
    }

    pub fn kill_pod(&self, label: &WidgetWebviewLabel) {
        self.pods.remove(label);
    }
}

pub struct WidgetPod {
    pub label: WidgetWebviewLabel,
    pub source: PodSource,

    window: Option<WidgetWebview>,
    _status: WidgetStatus,
    owner_hwnd: Option<isize>,

    live: Arc<tokio::sync::Notify>,
    liveness_prove_handle: Option<tokio::task::JoinHandle<()>>,
    hidden_suspend_handle: Option<tokio::task::JoinHandle<()>>,
    retries: Arc<AtomicU8>,
}

impl WidgetPod {
    fn create(label: WidgetWebviewLabel, owner_hwnd: Option<isize>, source: PodSource) -> Self {
        Self {
            label,
            source,
            window: None,
            _status: WidgetStatus::Pending,
            owner_hwnd,
            live: Arc::new(tokio::sync::Notify::new()),
            liveness_prove_handle: None,
            hidden_suspend_handle: None,
            retries: Arc::new(AtomicU8::new(0)),
        }
    }

    pub fn status(&self) -> &WidgetStatus {
        &self._status
    }

    pub fn hwnd(&self) -> Option<isize> {
        self.window.as_ref()?.0.hwnd().ok().map(|h| h.0 as isize)
    }

    pub fn set_status(&mut self, status: WidgetStatus) {
        log::trace!(target: &self.label.decoded, "status changed to: {status:?}");
        self._status = status;
        notify_widget_statuses_change();
    }

    pub fn is_ready(&self) -> bool {
        self.window.is_some() && self.status() == &WidgetStatus::Ready
    }

    pub fn soft_restart(&mut self) {
        if self.window.is_none() {
            // Pod was never started; leave it in Pending so run() can initialize it.
            return;
        }
        self.set_status(WidgetStatus::Restarting);
        if let Some(window) = &self.window {
            window.reload();
        }
    }

    fn run(&mut self, definition: &Widget) {
        if self.status() != &WidgetStatus::Pending {
            return;
        }

        self.set_status(WidgetStatus::Creating);
        let window = match WidgetWebview::create(definition, &self.label, self.owner_hwnd) {
            Ok(window) => window,
            Err(err) => {
                log::error!("Failed to create webview: {}", err);
                self.set_status(WidgetStatus::CrashedOnCreation);
                return;
            }
        };
        self.set_status(WidgetStatus::Mounting);

        let label = self.label.clone();
        window.0.on_window_event(move |event| {
            if let tauri::WindowEvent::Destroyed = event {
                // Defer window creation off the UI message-loop thread.
                // Calling start_all_webviews() (→ WidgetWebview::create → builder.build)
                // synchronously here triggers a re-entrant ZwUserDestroyWindow while the
                // message pump is still inside the destruction handler, which causes the
                // APPLICATION_HANG_ENDTASK_HungThreadIsIdle crash.
                let label = label.clone();
                std::thread::spawn(move || {
                    WIDGET_MANAGER.deployments.get(&label.widget_id, |deploy| {
                        deploy.kill_pod(&label);
                        deploy.reconcile();
                        // suspend_all() destroys every widget when game mode starts; respawning
                        // here would bring them back on top of the game. resume_all() starts
                        // them again once game mode ends.
                        if !deploy.definition.lazy
                            && !GAME_MODE_ACTIVE.load(std::sync::atomic::Ordering::Acquire)
                        {
                            deploy.start_all_webviews();
                        }
                    });
                });
            }
        });

        if definition.debug {
            window.0.open_devtools();
        }

        self.window = Some(window);
        notify_widget_statuses_change();

        // Third-party widgets may intentionally run background timers or audio work.
        // Only Seelen-owned internal widgets are eligible for automatic suspension.
        let suspend_when_hidden = matches!(
            definition.loader,
            WidgetLoader::Internal | WidgetLoader::InternalReact
        );
        self.start_hidden_webview_suspension(suspend_when_hidden);
        self.start_liveness_prove(suspend_when_hidden);
    }

    fn start_hidden_webview_suspension(&mut self, suspend_scripts: bool) {
        let Some(window) = &self.window else {
            return;
        };

        let native_window = window.0.clone();
        let webview = window.handle();
        let label = self.label.clone();

        let handle = get_tokio_handle().spawn(async move {
            let mut hidden_since: Option<Instant> = None;
            let mut low_memory_target_applied = false;

            loop {
                tokio::time::sleep(HIDDEN_WEBVIEW_SUSPEND_CHECK_INTERVAL).await;

                // A failed visibility query must be treated as visible. Never apply
                // memory pressure to a window whose visibility cannot be determined.
                let is_visible = native_window.is_visible().unwrap_or(true);
                if is_visible {
                    hidden_since = None;
                    if low_memory_target_applied {
                        webview::set_low_memory_target(&webview, false);
                        low_memory_target_applied = false;
                    }
                    continue;
                }

                // Never change renderer state while a widget is mounting/initializing.
                if !WIDGET_MANAGER.is_ready(&label) {
                    hidden_since = None;
                    continue;
                }

                let hidden_since = hidden_since.get_or_insert_with(Instant::now);
                if hidden_since.elapsed() < HIDDEN_WEBVIEW_SUSPEND_GRACE_PERIOD {
                    continue;
                }

                if suspend_scripts {
                    // Internal widgets have no required background work. TrySuspend
                    // pauses JS timers/animations and moves WebView2 to low memory.
                    webview::suspend_if_hidden(&webview);
                } else if !low_memory_target_applied {
                    // Third-party widgets may rely on timers, audio, or network work.
                    // Keep scripts running but ask WebView2 to trim inactive memory.
                    webview::set_low_memory_target(&webview, true);
                    low_memory_target_applied = true;
                }
            }
        });

        self.hidden_suspend_handle = Some(handle);
    }

    fn start_liveness_prove(&mut self, skip_hidden_internal_widgets: bool) {
        if let Some(window) = &self.window {
            let live = self.live.clone();
            window.0.listen("internal::liveness-pong", move |_event| {
                live.notify_waiters();
            });
        }

        let live = self.live.clone();
        let label = self.label.clone();
        let retries = self.retries.clone();
        let native_window = self.window.as_ref().map(|window| window.0.clone());

        let handle = get_tokio_handle().spawn(async move {
            let app = get_app_handle();
            let mut was_suspended = false;

            loop {
                tokio::time::sleep(LIVENESS_PROVE_INTERVAL).await;
                if !IS_INTERACTIVE_SESSION.load(std::sync::atomic::Ordering::Acquire) {
                    was_suspended = true;
                    continue;
                }

                // After session resume, reset state and wait for webview to finish reloading.
                if was_suspended {
                    was_suspended = false;
                    retries.store(0, std::sync::atomic::Ordering::SeqCst);
                    tokio::time::sleep(LIVENESS_RELOAD_GRACE_PERIOD).await;
                    continue;
                }

                // A suspended renderer cannot answer JavaScript liveness pings. Skip
                // probes while an internal widget is hidden; visibility restoration resumes
                // the renderer automatically and probes continue on the next interval.
                if skip_hidden_internal_widgets
                    && native_window
                        .as_ref()
                        .is_some_and(|window| !window.is_visible().unwrap_or(true))
                {
                    retries.store(0, std::sync::atomic::Ordering::SeqCst);
                    continue;
                }

                let _ = app.emit_to(&label.raw, "internal::liveness-ping", ());

                tokio::select! {
                    _ = live.notified() => {
                        // Widget is healthy: reset consecutive failure counter.
                        retries.store(0, std::sync::atomic::Ordering::SeqCst);
                    }
                    _ = tokio::time::sleep(LIVENESS_PROVE_WAIT_TIMEOUT) => {
                        let attempt = retries.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        log::warn!("Liveness prove failed for {label} (attempt {}/{LIVENESS_PROVE_MAX_RETRIES}), reloading webview.", attempt + 1);

                        if attempt < LIVENESS_PROVE_MAX_RETRIES {
                            WIDGET_MANAGER.deployments.get(&label.widget_id, |deployment| {
                                deployment.pods.get(&label, |pod| {
                                    pod.soft_restart();
                                });
                            });
                        } else {
                            log::error!("Liveness prove failed for {label} too many times, giving up.");
                            let lang = rust_i18n::locale();
                            let widget_name = RESOURCES
                                .widgets
                                .read_async(&label.widget_id, |_, w| {
                                    w.metadata.display_name.get(&lang).to_string()
                                })
                                .await
                                .unwrap_or_else(|| label.widget_id.to_string());
                            app.dialog()
                                .message(t!("widget_liveness.failed_description", widget_name = widget_name))
                                .title(t!("widget_liveness.failed_title"))
                                .kind(MessageDialogKind::Error)
                                .buttons(MessageDialogButtons::Ok)
                                .show(|_| {});
                            break;
                        }
                    }
                }
            }
        });

        self.liveness_prove_handle = Some(handle);
    }
}

impl Drop for WidgetPod {
    fn drop(&mut self) {
        log::trace!(target: &self.label.decoded, "dropped");
        if let Some(handle) = self.liveness_prove_handle.take() {
            handle.abort();
        }
        if let Some(handle) = self.hidden_suspend_handle.take() {
            handle.abort();
        }
    }
}

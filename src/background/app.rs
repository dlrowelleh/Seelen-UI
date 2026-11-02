use std::sync::{LazyLock, Mutex, atomic::{AtomicBool, Ordering}};

use slu_ipc::messages::SvcAction;
use tauri::{AppHandle, Emitter, Wry};
use windows::Win32::System::TaskScheduler::{ITaskService, TaskScheduler};

use seelen_core::state::shortcuts::resolve_shortcuts;

use crate::{
    APP_HANDLE,
    cli::ServicePipe,
    error::{Result, ResultLogExt},
    hook::register_win_hook,
    migrations::Migrations,
    modules::{
        apps::application::UserAppsManager,
        clipboard::infrastructure::init_clipboard_manager,
        start::application::StartMenuManager,
        user::infrastructure::reemit_user,
    },
    resources::RESOURCES,
    session::infrastructure::reemit_session,
    state::application::{AppSettings, FULL_STATE, initialize_user_resources_watcher},
    utils::discord::{start_discord_rpc, update_discord_rpc},
    widgets::{
        manager::WIDGET_MANAGER, popups::shortcut_conflicts::show_shortcut_conflict_popup,
        weg::SeelenWeg,
    },
    windows_api::{
        Com, ComThread,
        event_window::{IS_INTERACTIVE_SESSION, create_background_window},
    },
};

/// Tauri app handle
pub fn get_app_handle<'a>() -> &'a AppHandle<Wry> {
    APP_HANDLE
        .get()
        .expect("get_app_handle called but app is still not initialized")
}

pub fn emit_to_webviews<S>(event: &str, payload: S)
where
    S: serde::Serialize,
{
    if !IS_INTERACTIVE_SESSION.load(Ordering::Acquire) {
        return;
    }

    // serialized once and shared by both transports
    let payload = match serde_json::to_string(&payload) {
        Ok(payload) => payload,
        Err(err) => {
            log::error!("Failed to serialize event {event}: {err}");
            return;
        }
    };

    crate::server::events::EVENTS_HUB.publish(event, &payload);
    // kept for backward compatibility with widgets listening via the tauri api
    get_app_handle().emit_str(event, payload).log_error();
}

pub struct SeelenUI {}

// PackageCatalog is a WinRT event source. Keep its owning STA apartment and message pump
// alive for the lifetime of the app so install/uninstall notifications continue to arrive.
static START_MENU_COM_THREAD: LazyLock<Mutex<Option<ComThread<()>>>> =
    LazyLock::new(|| Mutex::new(None));
static TASKBAR_TRACKING_INIT_SCHEDULED: AtomicBool = AtomicBool::new(false);

/// Initializes taskbar-only tracking once, outside the app startup/settings thread.
fn schedule_taskbar_tracking_init() -> Result<()> {
    if TASKBAR_TRACKING_INIT_SCHEDULED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(());
    }

    if let Err(error) = std::thread::Builder::new()
        .name("TaskbarTracking".into())
        .spawn(UserAppsManager::init_taskbar_tracking)
    {
        TASKBAR_TRACKING_INIT_SCHEDULED.store(false, Ordering::Release);
        return Err(error.into());
    }

    Ok(())
}

/// Queue WinRT work on the long-lived Start-menu STA. PackageCatalog callbacks must never
/// move WinRT operations to an arbitrary std::thread without initializing its COM apartment.
pub(crate) fn execute_start_menu_com_task<F>(task: F) -> Result<()>
where
    F: FnOnce() + Send + 'static,
{
    let guard = START_MENU_COM_THREAD
        .lock()
        .map_err(|_| "Start-menu COM thread lock poisoned".to_string())?;
    let thread = guard
        .as_ref()
        .ok_or_else(|| "Start-menu COM thread is not initialized".to_string())?;
    thread.execute(move |_| task())
}

/* ============== Methods ============== */
impl SeelenUI {
    /// this fn prepares the app loading assets and settings in memory parallelly
    pub async fn pre_start() -> Result<()> {
        Migrations::run_all()?;

        let webview_check = tokio::task::spawn(async {
            crate::measure!(
                "Webview optimal state",
                crate::utils::integrity::check_for_webview_optimal_state().await
            )
        });

        crate::measure!("RESOURCES", RESOURCES.initialize().await);

        // Start-menu enumeration performs shell and WinRT work across the installed app set.
        // Keep it off the startup path; GetStartMenuItems returns the cached snapshot and the
        // manager emits ItemsRefreshed once the background index is ready. PackageCatalog also
        // needs its STA message pump to remain alive for install/uninstall callbacks.
        std::thread::Builder::new()
            .name("StartMenuIndex".into())
            .spawn(|| {
                let result = ComThread::spawn("Start Menu COM", || {
                    crate::measure!("START_MENU", StartMenuManager::initialize)?;
                    Ok(())
                });

                match result {
                    Ok(thread) => {
                        *START_MENU_COM_THREAD.lock().expect("start menu COM lock poisoned") =
                            Some(thread);
                    }
                    Err(err) => {
                        log::error!("Failed to initialize Start Menu index: {err}");
                    }
                }
            })?;

        crate::measure!("APP SETTINGS", FULL_STATE.load());

        if !webview_check.await.unwrap_or(false) {
            return Err("Webview optimal state check failed".into());
        }

        FULL_STATE.rcu(|state| {
            let mut state = state.cloned();
            state.complete_initialization(&RESOURCES);
            state
        });

        Ok(())
    }

    pub async fn start() -> Result<()> {
        let state = FULL_STATE.load();
        rust_i18n::set_locale(state.locale());

        if state.is_weg_enabled() {
            SeelenWeg::hide_native_taskbar();
        }

        WIDGET_MANAGER.reconcile()?;
        create_background_window()?;
        register_win_hook()?;

        // Taskbar tracking uses the background window's shell-hook message pump, so it must
        // only be initialized after create_background_window() has completed.
        if state.is_weg_enabled() {
            schedule_taskbar_tracking_init()?;
        }

        // The first Alt+Tab request otherwise triggers a full synchronous Win32 window
        // enumeration and process/AppUserModelID resolution on the hotkey path. Warm the
        // window cache and WinEvent subscriptions in the background when the switcher is enabled.
        if state.settings.is_widget_enabled(&"@seelen/task-switcher".into()) {
            std::thread::Builder::new()
                .name("UserAppWindowsWarmup".into())
                .spawn(|| {
                    let _ = UserAppsManager::instance();
                })?;
        }

        std::thread::spawn(init_clipboard_manager);

        start_discord_rpc()?;

        initialize_user_resources_watcher()?;

        let widgets = RESOURCES.widgets();
        let widget_refs: Vec<_> = widgets.iter().map(|w| w.as_ref()).collect();
        let (resolved, _) = resolve_shortcuts(&state.settings, &widget_refs);

        if !crate::cli::shortcuts::SHORTCUTS_PAUSED.load(std::sync::atomic::Ordering::Acquire) {
            ServicePipe::request(SvcAction::SetShortcuts(resolved))?;
        }

        Ok(())
    }

    pub fn on_settings_change(state: &AppSettings) -> Result<()> {
        update_discord_rpc(state.settings.drpc).log_error();
        rust_i18n::set_locale(state.locale());

        let widgets = RESOURCES.widgets();
        let widget_refs: Vec<_> = widgets.iter().map(|w| w.as_ref()).collect();
        let (resolved, has_conflicts) = resolve_shortcuts(&state.settings, &widget_refs);
        if has_conflicts {
            show_shortcut_conflict_popup().log_error();
        }
        if !crate::cli::shortcuts::SHORTCUTS_PAUSED.load(std::sync::atomic::Ordering::Acquire) {
            ServicePipe::request(SvcAction::SetShortcuts(resolved))?;
        }

        if state.is_weg_enabled() {
            SeelenWeg::hide_native_taskbar();
            schedule_taskbar_tracking_init()?;
        } else {
            SeelenWeg::restore_native_taskbar()?;
        }

        // Re-emit user and session so streaming mode redaction takes effect immediately.
        reemit_user();
        reemit_session();
        Ok(())
    }

    pub fn is_auto_start_enabled() -> Result<bool> {
        Com::run_with_context(|| unsafe {
            let task_service: ITaskService = Com::create_instance(&TaskScheduler)?;
            task_service.Connect(
                &Default::default(),
                &Default::default(),
                &Default::default(),
                &Default::default(),
            )?;
            let is_task_enabled = task_service
                .GetFolder(&"\\Seelen".into())
                .and_then(|folder| folder.GetTask(&"Seelen UI Service".into()))
                .and_then(|task| task.Definition())
                .and_then(|definition| definition.Triggers())
                .and_then(|triggers| triggers.get_Item(1))
                .is_ok();
            Ok(is_task_enabled)
        })
    }

    pub fn set_auto_start(enabled: bool) -> Result<()> {
        ServicePipe::request(SvcAction::SetStartup(enabled))
    }
}

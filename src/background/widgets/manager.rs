use std::sync::{
    LazyLock,
    atomic::{AtomicBool, Ordering},
};

use seelen_core::{
    resource::WidgetId,
    state::{WidgetLoader, WidgetStatus},
};

use crate::{
    error::{Result, ResultLogExt},
    modules::monitors::MonitorManager,
    resources::RESOURCES,
    state::application::FULL_STATE,
    utils::lock_free::SyncHashMap,
    widgets::{WidgetWebviewLabel, loader::WidgetDeployment},
};

pub static WIDGET_MANAGER: LazyLock<WidgetManager> = LazyLock::new(WidgetManager::create);
pub static GAME_MODE_ACTIVE: AtomicBool = AtomicBool::new(false);

pub struct WidgetManager {
    /// group of widgets instances by widget resource id
    pub deployments: SyncHashMap<WidgetId, WidgetDeployment>,
}

impl WidgetManager {
    fn create() -> Self {
        let sub_id = MonitorManager::subscribe(|_event| {
            WIDGET_MANAGER.reconcile().log_error();
        });
        MonitorManager::set_event_handler_priority(&sub_id, 1);
        Self {
            deployments: SyncHashMap::new(),
        }
    }

    pub fn is_ready(&self, label: &WidgetWebviewLabel) -> bool {
        self.deployments
            .get(&label.widget_id, |deploy| {
                deploy.pods.any(|(key, pod)| key == label && pod.is_ready())
            })
            .unwrap_or(false)
    }

    pub fn set_status(&self, label: &WidgetWebviewLabel, status: WidgetStatus) {
        self.deployments.get(&label.widget_id, |deploy| {
            deploy.pods.get(label, |instance| {
                instance.set_status(status);
            });
        });
    }

    pub fn suspend_all(&self) {
        GAME_MODE_ACTIVE.store(true, Ordering::Release);
        self.deployments.for_each(|(_, deploy)| {
            deploy.pods.clear();
        });
    }

    pub fn resume_all(&self) -> Result<()> {
        GAME_MODE_ACTIVE.store(false, Ordering::Release);
        self.reconcile()
    }

    pub fn reconcile(&self) -> Result<()> {
        // remove deleted resources
        self.deployments
            .retain(|(key, _)| RESOURCES.widgets.contains_sync(key));

        let mut filtered = Vec::new();
        RESOURCES.widgets.iter_sync(|k, w| {
            if w.loader != WidgetLoader::Legacy {
                filtered.push((k.clone(), w.clone()));
            }
            true
        });

        let state = FULL_STATE.load();
        for (id, widget) in filtered {
            if !state.is_widget_enabled(&id) {
                self.deployments.remove(&id);
                continue;
            }

            if !self.deployments.contains_key(&id) {
                self.deployments
                    .upsert(id.clone(), WidgetDeployment::new(widget));
            }
        }

        // Coalesce bursts of monitor/resource changes into a single worker.
        schedule_deployment_reconcile();

        Ok(())
    }
}

static RECONCILE_RUNNING: AtomicBool = AtomicBool::new(false);
static RECONCILE_PENDING: AtomicBool = AtomicBool::new(false);

fn schedule_deployment_reconcile() {
    RECONCILE_PENDING.store(true, Ordering::Release);
    if RECONCILE_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    std::thread::spawn(|| {
        loop {
            if RECONCILE_PENDING.swap(false, Ordering::AcqRel) {
                // A malformed deployment or an unexpected platform error must not strand
                // RECONCILE_RUNNING=true forever. Catch the panic, log it, and let the
                // state machine return to idle so later settings/monitor changes can retry.
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(reconcile_deployments))
                    .is_err()
                {
                    log::error!("Widget deployment reconciliation panicked; future requests will retry");
                }
                continue;
            }

            RECONCILE_RUNNING.store(false, Ordering::Release);

            // If a request raced with the transition to idle, either this worker
            // reclaims the running flag or the requesting thread starts a worker.
            if RECONCILE_PENDING.load(Ordering::Acquire)
                && RECONCILE_RUNNING
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                continue;
            }
            break;
        }
    });
}

fn reconcile_deployments() {
    // Keep visually important widgets first, but don't reconcile them twice.
    let priorities = [
        WidgetId::known_wall(),
        WidgetId::known_toolbar(),
        WidgetId::known_weg(),
    ];

    for priority in &priorities {
        WIDGET_MANAGER.deployments.get(priority, |deployment| {
            reconcile_deployment(deployment);
        });
    }

    WIDGET_MANAGER.deployments.for_each(|(id, deployment)| {
        if !priorities.contains(id) {
            reconcile_deployment(deployment);
        }
    });
}

fn reconcile_deployment(deployment: &WidgetDeployment) {
    deployment.reconcile();
    if !deployment.definition.lazy && !GAME_MODE_ACTIVE.load(Ordering::Acquire) {
        deployment.start_all_webviews();
    }
}

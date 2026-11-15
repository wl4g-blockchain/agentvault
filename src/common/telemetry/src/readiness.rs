// SPDX-License-Identifier: GPL-3.0-only
//! Process-local lifecycle health; contains no request identity or user context.
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;

struct Component {
    business: bool,
    ready: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
}
static COMPONENTS: OnceLock<Mutex<BTreeMap<String, Component>>> = OnceLock::new();

pub struct RuntimeReadiness;
pub struct ReadinessGuard {
    ready: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    completed: AtomicBool,
}

impl RuntimeReadiness {
    pub fn business(name: &str, ready: bool) {
        let mut components = COMPONENTS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let component = components.entry(name.into()).or_insert_with(|| Component {
            business: true,
            ready: Arc::new(AtomicBool::new(false)),
            alive: Arc::new(AtomicBool::new(true)),
        });
        component.ready.store(ready, Ordering::Release);
    }
    pub fn task(name: impl Into<String>) -> ReadinessGuard {
        let ready = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        COMPONENTS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                name.into(),
                Component {
                    business: false,
                    ready: ready.clone(),
                    alive: alive.clone(),
                },
            );
        ReadinessGuard {
            ready,
            alive,
            completed: AtomicBool::new(false),
        }
    }
    pub fn snapshot() -> (bool, BTreeMap<String, bool>) {
        let components = COMPONENTS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ready = components.values().any(|value| value.business)
            && components
                .values()
                .all(|value| value.ready.load(Ordering::Acquire) && value.alive.load(Ordering::Acquire));
        (
            ready,
            components
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.ready.load(Ordering::Acquire) && value.alive.load(Ordering::Acquire),
                    )
                })
                .collect(),
        )
    }
    /// Only unexpected task termination is fatal. Startup and dependency outages
    /// affect readiness while the process remains capable of recovering itself.
    pub fn liveness_snapshot() -> (bool, BTreeMap<String, bool>) {
        let components = COMPONENTS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tasks = components
            .iter()
            .filter(|(_, value)| !value.business)
            .map(|(key, value)| (key.clone(), value.alive.load(Ordering::Acquire)))
            .collect::<BTreeMap<_, _>>();
        (tasks.values().all(|alive| *alive), tasks)
    }
}
impl ReadinessGuard {
    pub fn set(&self, ready: bool) {
        self.ready
            .store(ready && self.alive.load(Ordering::Acquire), Ordering::Release);
    }
    /// Permanently fail this task incarnation; only registering a replacement
    /// task can restore its liveness. A transient dependency error uses set(false).
    pub fn fail(&self) {
        self.alive.store(false, Ordering::Release);
        self.ready.store(false, Ordering::Release);
    }
    /// A finite successful job may stay healthy after its worker future returns.
    pub fn complete(self) {
        self.finish();
    }
    /// Mark verified graceful completion when the guard is held by shared state.
    /// This never clears an earlier fatal failure of the same task incarnation.
    pub fn finish(&self) {
        self.set(true);
        self.completed.store(true, Ordering::Release);
    }
}
impl Drop for ReadinessGuard {
    fn drop(&mut self) {
        if !self.completed.load(Ordering::Acquire) {
            self.fail();
        }
    }
}

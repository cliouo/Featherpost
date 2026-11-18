//! scheduler: shared timers for polling and future time-based plugins.

use std::any::TypeId;
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use cordis::{BoxFuture, Context, Plugin, PluginError, Service};
use futures::FutureExt;
use serde::Deserialize;
use tokio::runtime::Handle;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

fn default_jitter_secs() -> u64 {
    1
}

#[derive(Clone, Debug, Deserialize)]
pub struct SchedulerConfig {
    /// Default additive jitter for interval jobs.
    #[serde(default = "default_jitter_secs")]
    pub jitter_secs: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            jitter_secs: default_jitter_secs(),
        }
    }
}

impl Service for SchedulerConfig {
    fn name(&self) -> &'static str {
        "scheduler.config"
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ScheduleId(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScheduleScope {
    Global,
    Account(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitialDelay {
    Immediate,
    AfterPeriod,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JitterPolicy {
    Default,
    None,
    Bounded(Duration),
}

#[derive(Clone, Copy, Debug)]
pub enum ScheduleSpec {
    Interval {
        every: Duration,
        initial: InitialDelay,
        jitter: JitterPolicy,
    },
    After(Duration),
    At(Instant),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobContext {
    pub id: ScheduleId,
    pub account: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScheduleError {
    Cancelled,
    Invalid(String),
    Closed,
    Callback(String),
}

impl ScheduleError {
    pub fn callback(message: impl Into<String>) -> Self {
        Self::Callback(message.into())
    }
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "schedule cancelled"),
            Self::Invalid(message) => write!(f, "invalid schedule: {message}"),
            Self::Closed => write!(f, "scheduler is closed"),
            Self::Callback(message) => write!(f, "scheduled callback failed: {message}"),
        }
    }
}

impl std::error::Error for ScheduleError {}

type Callback =
    Arc<dyn Fn(JobContext) -> BoxFuture<'static, Result<(), ScheduleError>> + Send + Sync>;

trait Timer: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep_until(&self, deadline: Instant) -> BoxFuture<'static>;
}

struct TokioTimer;

impl Timer for TokioTimer {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep_until(&self, deadline: Instant) -> BoxFuture<'static> {
        Box::pin(tokio::time::sleep_until(deadline))
    }
}

trait JitterSource: Send + Sync {
    fn sample(&self, maximum: Duration) -> Duration;
}

struct RandomJitter;

impl JitterSource for RandomJitter {
    fn sample(&self, maximum: Duration) -> Duration {
        let max_nanos = maximum.as_nanos().min(u64::MAX as u128) as u64;
        if max_nanos == 0 {
            return Duration::ZERO;
        }
        let random = getrandom::u64().unwrap_or(0);
        Duration::from_nanos(random % max_nanos.saturating_add(1))
    }
}

struct JobEntry {
    scope: ScheduleScope,
    cancel: watch::Sender<bool>,
    task: JoinHandle<()>,
}

struct SchedulerInner {
    config: SchedulerConfig,
    handle: Handle,
    timer: Arc<dyn Timer>,
    jitter: Arc<dyn JitterSource>,
    next_id: AtomicU64,
    closed: AtomicBool,
    jobs: Mutex<HashMap<ScheduleId, JobEntry>>,
}

#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<SchedulerInner>,
}

impl Service for Scheduler {
    fn name(&self) -> &'static str {
        "scheduler"
    }
}

impl Scheduler {
    pub fn new(config: SchedulerConfig) -> Result<Self, ScheduleError> {
        let handle = Handle::try_current()
            .map_err(|_| ScheduleError::Invalid("scheduler requires a Tokio runtime".into()))?;
        Ok(Self::with_components(
            config,
            handle,
            Arc::new(TokioTimer),
            Arc::new(RandomJitter),
        ))
    }

    fn with_components(
        config: SchedulerConfig,
        handle: Handle,
        timer: Arc<dyn Timer>,
        jitter: Arc<dyn JitterSource>,
    ) -> Self {
        Self {
            inner: Arc::new(SchedulerInner {
                config,
                handle,
                timer,
                jitter,
                next_id: AtomicU64::new(1),
                closed: AtomicBool::new(false),
                jobs: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn schedule_global<F>(
        &self,
        spec: ScheduleSpec,
        callback: F,
    ) -> Result<ScheduleHandle, ScheduleError>
    where
        F: Fn(JobContext) -> BoxFuture<'static, Result<(), ScheduleError>> + Send + Sync + 'static,
    {
        self.schedule(ScheduleScope::Global, spec, Arc::new(callback))
    }

    pub fn schedule_account<F>(
        &self,
        account: impl Into<String>,
        spec: ScheduleSpec,
        callback: F,
    ) -> Result<ScheduleHandle, ScheduleError>
    where
        F: Fn(JobContext) -> BoxFuture<'static, Result<(), ScheduleError>> + Send + Sync + 'static,
    {
        let account = account.into();
        if account.is_empty() {
            return Err(ScheduleError::Invalid("account must not be empty".into()));
        }
        self.schedule(ScheduleScope::Account(account), spec, Arc::new(callback))
    }

    fn schedule(
        &self,
        scope: ScheduleScope,
        spec: ScheduleSpec,
        callback: Callback,
    ) -> Result<ScheduleHandle, ScheduleError> {
        validate_spec(spec)?;
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(ScheduleError::Closed);
        }

        let id = ScheduleId(self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let (cancel, cancel_rx) = watch::channel(false);
        let (start, start_rx) = oneshot::channel();
        let inner = Arc::clone(&self.inner);
        let task_scope = scope.clone();
        let task = self.inner.handle.spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            let outcome = std::panic::AssertUnwindSafe(run_job(
                Arc::clone(&inner),
                id,
                task_scope,
                spec,
                callback,
                cancel_rx,
            ))
            .catch_unwind()
            .await;
            if outcome.is_err() {
                eprintln!("[maild/scheduler] job {:?} panicked", id);
            }
            inner.jobs.lock().unwrap().remove(&id);
        });

        let entry = JobEntry {
            scope,
            cancel,
            task,
        };
        let mut jobs = self.inner.jobs.lock().unwrap();
        if self.inner.closed.load(Ordering::Acquire) {
            let _ = entry.cancel.send_replace(true);
            entry.task.abort();
            return Err(ScheduleError::Closed);
        }
        jobs.insert(id, entry);
        drop(jobs);
        let _ = start.send(());
        Ok(ScheduleHandle {
            scheduler: Arc::downgrade(&self.inner),
            id: Some(id),
        })
    }

    pub fn cancel(&self, id: ScheduleId) -> bool {
        cancel_inner(&self.inner, id)
    }

    pub fn cancel_account(&self, account: &str) -> usize {
        let ids = {
            let jobs = self.inner.jobs.lock().unwrap();
            jobs.iter()
                .filter_map(|(id, entry)| match &entry.scope {
                    ScheduleScope::Account(value) if value == account => Some(*id),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        ids.into_iter()
            .filter(|id| cancel_inner(&self.inner, *id))
            .count()
    }

    pub async fn shutdown(&self) {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let entries = {
            let mut jobs = self.inner.jobs.lock().unwrap();
            jobs.drain().map(|(_, entry)| entry).collect::<Vec<_>>()
        };
        for entry in &entries {
            let _ = entry.cancel.send_replace(true);
        }
        for entry in entries {
            let _ = entry.task.await;
        }
    }

    #[cfg(test)]
    fn job_count(&self) -> usize {
        self.inner.jobs.lock().unwrap().len()
    }
}

pub struct ScheduleHandle {
    scheduler: Weak<SchedulerInner>,
    id: Option<ScheduleId>,
}

impl ScheduleHandle {
    pub fn id(&self) -> ScheduleId {
        self.id.expect("active schedule handle")
    }

    pub fn cancel(&mut self) -> bool {
        let Some(id) = self.id.take() else {
            return false;
        };
        self.scheduler
            .upgrade()
            .is_some_and(|scheduler| cancel_inner(&scheduler, id))
    }
}

impl Drop for ScheduleHandle {
    fn drop(&mut self) {
        let _ = self.cancel();
    }
}

fn cancel_inner(inner: &Arc<SchedulerInner>, id: ScheduleId) -> bool {
    let jobs = inner.jobs.lock().unwrap();
    let Some(entry) = jobs.get(&id) else {
        return false;
    };
    if *entry.cancel.borrow() {
        return false;
    }
    let _ = entry.cancel.send_replace(true);
    true
}

fn validate_spec(spec: ScheduleSpec) -> Result<(), ScheduleError> {
    if let ScheduleSpec::Interval { every, .. } = spec {
        if every.is_zero() {
            return Err(ScheduleError::Invalid(
                "interval must be greater than zero".into(),
            ));
        }
    }
    Ok(())
}

async fn run_job(
    inner: Arc<SchedulerInner>,
    id: ScheduleId,
    scope: ScheduleScope,
    spec: ScheduleSpec,
    callback: Callback,
    mut cancel: watch::Receiver<bool>,
) {
    let context = JobContext {
        id,
        account: match &scope {
            ScheduleScope::Global => None,
            ScheduleScope::Account(account) => Some(account.clone()),
        },
    };

    match spec {
        ScheduleSpec::Interval {
            every,
            initial,
            jitter,
        } => {
            if initial == InitialDelay::AfterPeriod
                && wait_delay(&inner, every + sample_jitter(&inner, jitter), &mut cancel).await
            {
                return;
            }
            loop {
                let Some(result) = invoke_callback(&callback, context.clone(), &mut cancel).await
                else {
                    return;
                };
                match result {
                    Ok(()) => {}
                    Err(ScheduleError::Cancelled) => return,
                    Err(error) => eprintln!(
                        "[maild/scheduler] job {:?} account {:?}: {}",
                        id, context.account, error
                    ),
                }
                if wait_delay(&inner, every + sample_jitter(&inner, jitter), &mut cancel).await {
                    return;
                }
            }
        }
        ScheduleSpec::After(delay) => {
            if !wait_delay(&inner, delay, &mut cancel).await {
                if let Some(result) = invoke_callback(&callback, context.clone(), &mut cancel).await
                {
                    log_one_shot_error(id, &context, result);
                }
            }
        }
        ScheduleSpec::At(deadline) => {
            if !wait_until(&inner, deadline, &mut cancel).await {
                if let Some(result) = invoke_callback(&callback, context.clone(), &mut cancel).await
                {
                    log_one_shot_error(id, &context, result);
                }
            }
        }
    }
}

async fn invoke_callback(
    callback: &Callback,
    context: JobContext,
    cancel: &mut watch::Receiver<bool>,
) -> Option<Result<(), ScheduleError>> {
    tokio::select! {
        result = callback(context) => Some(result),
        _ = cancel.changed() => None,
    }
}

fn log_one_shot_error(id: ScheduleId, context: &JobContext, result: Result<(), ScheduleError>) {
    if let Err(error) = result {
        if error != ScheduleError::Cancelled {
            eprintln!(
                "[maild/scheduler] one-shot {:?} account {:?}: {}",
                id, context.account, error
            );
        }
    }
}

fn sample_jitter(inner: &SchedulerInner, policy: JitterPolicy) -> Duration {
    let maximum = match policy {
        JitterPolicy::Default => Duration::from_secs(inner.config.jitter_secs),
        JitterPolicy::None => Duration::ZERO,
        JitterPolicy::Bounded(maximum) => maximum,
    };
    inner.jitter.sample(maximum)
}

async fn wait_delay(
    inner: &SchedulerInner,
    delay: Duration,
    cancel: &mut watch::Receiver<bool>,
) -> bool {
    wait_until(inner, inner.timer.now() + delay, cancel).await
}

async fn wait_until(
    inner: &SchedulerInner,
    deadline: Instant,
    cancel: &mut watch::Receiver<bool>,
) -> bool {
    if *cancel.borrow() {
        return true;
    }
    tokio::select! {
        _ = inner.timer.sleep_until(deadline) => false,
        _ = cancel.changed() => true,
    }
}

pub struct SchedulerPlugin;

impl Plugin for SchedulerPlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<SchedulerConfig>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let config = ctx.get::<SchedulerConfig>().expect("inject ready");
        let scheduler = Scheduler::new(config.as_ref().clone())?;
        ctx.provide::<Scheduler>(Arc::new(scheduler.clone()));
        ctx.effect_async(async move {
            scheduler.shutdown().await;
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::oneshot;

    struct ManualState {
        now: Instant,
        sleepers: Vec<(Instant, oneshot::Sender<()>)>,
    }

    struct ManualTimer {
        state: Mutex<ManualState>,
    }

    impl ManualTimer {
        fn new() -> Self {
            Self {
                state: Mutex::new(ManualState {
                    now: Instant::now(),
                    sleepers: Vec::new(),
                }),
            }
        }

        fn advance(&self, duration: Duration) {
            let ready = {
                let mut state = self.state.lock().unwrap();
                state.now += duration;
                let now = state.now;
                let mut ready = Vec::new();
                let mut pending = Vec::new();
                for (deadline, sender) in state.sleepers.drain(..) {
                    if deadline <= now {
                        ready.push(sender);
                    } else {
                        pending.push((deadline, sender));
                    }
                }
                state.sleepers = pending;
                ready
            };
            for sender in ready {
                let _ = sender.send(());
            }
        }
    }

    impl Timer for ManualTimer {
        fn now(&self) -> Instant {
            self.state.lock().unwrap().now
        }

        fn sleep_until(&self, deadline: Instant) -> BoxFuture<'static> {
            let mut state = self.state.lock().unwrap();
            if deadline <= state.now {
                return Box::pin(async {});
            }
            let (sender, receiver) = oneshot::channel();
            state.sleepers.push((deadline, sender));
            Box::pin(async move {
                let _ = receiver.await;
            })
        }
    }

    struct FixedJitter(Duration);

    impl JitterSource for FixedJitter {
        fn sample(&self, maximum: Duration) -> Duration {
            self.0.min(maximum)
        }
    }

    fn scheduler(timer: Arc<ManualTimer>, jitter: Duration) -> Scheduler {
        Scheduler::with_components(
            SchedulerConfig { jitter_secs: 5 },
            Handle::current(),
            timer,
            Arc::new(FixedJitter(jitter)),
        )
    }

    async fn settle() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn interval_waits_then_repeats_without_overlap() {
        let timer = Arc::new(ManualTimer::new());
        let scheduler = scheduler(Arc::clone(&timer), Duration::ZERO);
        let running = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = watch::channel(false);
        let running2 = Arc::clone(&running);
        let maximum2 = Arc::clone(&maximum);
        let calls2 = Arc::clone(&calls);
        let release_rx2 = release_rx.clone();
        let _handle = scheduler
            .schedule_global(
                ScheduleSpec::Interval {
                    every: Duration::from_secs(10),
                    initial: InitialDelay::AfterPeriod,
                    jitter: JitterPolicy::None,
                },
                move |_| {
                    let running = Arc::clone(&running2);
                    let maximum = Arc::clone(&maximum2);
                    let calls = Arc::clone(&calls2);
                    let mut release = release_rx2.clone();
                    Box::pin(async move {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        maximum.fetch_max(now, Ordering::SeqCst);
                        calls.fetch_add(1, Ordering::SeqCst);
                        let _ = release.wait_for(|value| *value).await;
                        running.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    })
                },
            )
            .unwrap();

        settle().await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        timer.advance(Duration::from_secs(10));
        settle().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        timer.advance(Duration::from_secs(100));
        settle().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1, "callback does not overlap");
        let _ = release_tx.send_replace(true);
        settle().await;
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_shot_runs_once_and_is_removed() {
        let timer = Arc::new(ManualTimer::new());
        let scheduler = scheduler(Arc::clone(&timer), Duration::ZERO);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = Arc::clone(&calls);
        let _handle = scheduler
            .schedule_global(ScheduleSpec::After(Duration::from_secs(3)), move |_| {
                let calls = Arc::clone(&calls2);
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
            .unwrap();
        settle().await;
        timer.advance(Duration::from_secs(3));
        settle().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(scheduler.job_count(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bounded_jitter_delays_interval() {
        let timer = Arc::new(ManualTimer::new());
        let scheduler = scheduler(Arc::clone(&timer), Duration::from_secs(2));
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = Arc::clone(&calls);
        let _handle = scheduler
            .schedule_global(
                ScheduleSpec::Interval {
                    every: Duration::from_secs(10),
                    initial: InitialDelay::AfterPeriod,
                    jitter: JitterPolicy::Bounded(Duration::from_secs(3)),
                },
                move |_| {
                    let calls = Arc::clone(&calls2);
                    Box::pin(async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                },
            )
            .unwrap();
        settle().await;
        timer.advance(Duration::from_secs(11));
        settle().await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        timer.advance(Duration::from_secs(1));
        settle().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn callback_errors_continue_but_cancelled_stops() {
        let timer = Arc::new(ManualTimer::new());
        let scheduler = scheduler(Arc::clone(&timer), Duration::ZERO);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = Arc::clone(&calls);
        let _handle = scheduler
            .schedule_global(
                ScheduleSpec::Interval {
                    every: Duration::from_secs(1),
                    initial: InitialDelay::Immediate,
                    jitter: JitterPolicy::None,
                },
                move |_| {
                    let call = calls2.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async move {
                        if call == 0 {
                            Err(ScheduleError::callback("temporary"))
                        } else {
                            Err(ScheduleError::Cancelled)
                        }
                    })
                },
            )
            .unwrap();
        settle().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        timer.advance(Duration::from_secs(1));
        settle().await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(scheduler.job_count(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_is_idempotent_and_account_exact() {
        let timer = Arc::new(ManualTimer::new());
        let scheduler = scheduler(timer, Duration::ZERO);
        let noop = |_| Box::pin(async { Ok(()) }) as BoxFuture<'static, _>;
        let mut alpha = scheduler
            .schedule_account("alpha", ScheduleSpec::After(Duration::from_secs(5)), noop)
            .unwrap();
        let _alphabet = scheduler
            .schedule_account(
                "alphabet",
                ScheduleSpec::After(Duration::from_secs(5)),
                noop,
            )
            .unwrap();
        assert_eq!(scheduler.cancel_account("alpha"), 1);
        assert!(!alpha.cancel());
        settle().await;
        assert_eq!(scheduler.job_count(), 1);
        assert_eq!(scheduler.cancel_account("alpha"), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_waits_and_rejects_new_jobs() {
        let timer = Arc::new(ManualTimer::new());
        let scheduler = scheduler(timer, Duration::ZERO);
        let _handle = scheduler
            .schedule_global(ScheduleSpec::After(Duration::from_secs(100)), |_| {
                Box::pin(async { Ok(()) })
            })
            .unwrap();
        scheduler.shutdown().await;
        assert_eq!(scheduler.job_count(), 0);
        assert!(matches!(
            scheduler.schedule_global(ScheduleSpec::After(Duration::ZERO), |_| {
                Box::pin(async { Ok(()) })
            }),
            Err(ScheduleError::Closed)
        ));
        scheduler.shutdown().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalid_schedules_are_rejected() {
        let timer = Arc::new(ManualTimer::new());
        let scheduler = scheduler(timer, Duration::ZERO);
        assert!(matches!(
            scheduler.schedule_global(
                ScheduleSpec::Interval {
                    every: Duration::ZERO,
                    initial: InitialDelay::Immediate,
                    jitter: JitterPolicy::None,
                },
                |_| Box::pin(async { Ok(()) })
            ),
            Err(ScheduleError::Invalid(_))
        ));
        assert!(matches!(
            scheduler.schedule_account("", ScheduleSpec::After(Duration::ZERO), |_| {
                Box::pin(async { Ok(()) })
            }),
            Err(ScheduleError::Invalid(_))
        ));
    }
}

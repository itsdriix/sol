#![allow(clippy::arithmetic_side_effects)]
//! Transaction scheduling code.
//!
//! This crate implements 3 solana-runtime traits (`InstalledScheduler`, `UninstalledScheduler` and
//! `InstalledSchedulerPool`) to provide a concrete transaction scheduling implementation
//! (including executing txes and committing tx results).
//!
//! At the highest level, this crate takes `SanitizedTransaction`s via its `schedule_execution()`
//! and commits any side-effects (i.e. on-chain state changes) into the associated `Bank` via
//! `solana-ledger`'s helper function called `execute_batch()`.

use {
    assert_matches::assert_matches,
    crossbeam_channel::{
        bounded, disconnected, never, select_biased, unbounded, Receiver, RecvError,
        RecvTimeoutError, Sender, TryRecvError,
    },
    log::*,
    solana_ledger::blockstore_processor::{
        execute_batch, TransactionBatchWithIndexes, TransactionStatusSender,
    },
    solana_program_runtime::timings::ExecuteTimings,
    solana_runtime::{
        bank::Bank,
        installed_scheduler_pool::{
            DefaultScheduleExecutionArg, InstalledScheduler, InstalledSchedulerPool,
            InstalledSchedulerPoolArc, ResultWithTimings, ScheduleExecutionArg, SchedulerId,
            SchedulingContext, UninstalledScheduler, UninstalledSchedulerBox,
            WithTransactionAndIndex,
        },
        prioritization_fee_cache::PrioritizationFeeCache,
    },
    solana_sdk::{
        pubkey::Pubkey,
        slot_history::Slot,
        transaction::{Result, SanitizedTransaction},
    },
    solana_unified_scheduler_logic::{Page, SchedulingStateMachine, Task},
    solana_vote::vote_sender_types::ReplayVoteSender,
    std::{
        fmt::Debug,
        sync::{
            atomic::{AtomicU64, Ordering::Relaxed},
            Arc, Mutex, RwLock, RwLockReadGuard, Weak,
        },
        thread::{sleep, JoinHandle},
        time::{Duration, Instant, SystemTime},
    },
};

type AtomicSchedulerId = AtomicU64;

// SchedulerPool must be accessed as a dyn trait from solana-runtime, because SchedulerPool
// contains some internal fields, whose types aren't available in solana-runtime (currently
// TransactionStatusSender; also, PohRecorder in the future)...
#[derive(Debug)]
pub struct SchedulerPool<
    S: SpawnableScheduler<TH, SEA>,
    TH: TaskHandler<SEA>,
    SEA: ScheduleExecutionArg,
> {
    scheduler_inners: Mutex<Vec<S::Inner>>,
    handler_context: HandlerContext,
    // weak_self could be elided by changing InstalledScheduler::take_scheduler()'s receiver to
    // Arc<Self> from &Self, because SchedulerPool is used as in the form of Arc<SchedulerPool>
    // almost always. But, this would cause wasted and noisy Arc::clone()'s at every call sites.
    //
    // Alternatively, `impl InstalledScheduler for Arc<SchedulerPool>` approach could be explored
    // but it entails its own problems due to rustc's coherence and necessitated newtype with the
    // type graph of InstalledScheduler being quite elaborate.
    //
    // After these considerations, this weak_self approach is chosen at the cost of some additional
    // memory increase.
    weak_self: Weak<Self>,
    next_scheduler_id: AtomicSchedulerId,
    // prune schedulers, stop idling scheduler's threads, sanity check on the
    // address book after scheduler is returned.
    watchdog_sender: Sender<Weak<RwLock<ThreadManager<S, TH, SEA>>>>,
    _watchdog_thread: JoinHandle<()>,
}

#[derive(Debug)]
pub struct HandlerContext {
    log_messages_bytes_limit: Option<usize>,
    transaction_status_sender: Option<TransactionStatusSender>,
    replay_vote_sender: Option<ReplayVoteSender>,
    prioritization_fee_cache: Arc<PrioritizationFeeCache>,
}

pub type DefaultSchedulerPool = SchedulerPool<
    PooledScheduler<DefaultTaskHandler, DefaultScheduleExecutionArg>,
    DefaultTaskHandler,
    DefaultScheduleExecutionArg,
>;

struct WatchedThreadManager<S, TH, SEA>
where
    S: SpawnableScheduler<TH, SEA>,
    TH: TaskHandler<SEA>,
    SEA: ScheduleExecutionArg,
{
    thread_manager: Weak<RwLock<ThreadManager<S, TH, SEA>>>,
    tick: u64,
    updated_at: Instant,
}

impl<S, TH, SEA> WatchedThreadManager<S, TH, SEA>
where
    S: SpawnableScheduler<TH, SEA>,
    TH: TaskHandler<SEA>,
    SEA: ScheduleExecutionArg,
{
    fn new(thread_manager: Weak<RwLock<ThreadManager<S, TH, SEA>>>) -> Self {
        Self {
            thread_manager,
            tick: 0,
            updated_at: Instant::now(),
        }
    }

    fn retire_if_stale(&mut self) -> bool {
        let Some(thread_manager) = self.thread_manager.upgrade() else {
            return false;
        };
        let Some(tid) = thread_manager.read().unwrap().active_tid_if_not_primary() else {
            self.tick = 0;
            self.updated_at = Instant::now();
            return true;
        };

        let pid = std::process::id();
        let task = procfs::process::Process::new(pid.try_into().unwrap())
            .unwrap()
            .task_from_tid(tid)
            .unwrap();
        let stat = task.stat().unwrap();
        let current_tick = stat.utime + stat.stime;
        if current_tick > self.tick {
            self.tick = current_tick;
            self.updated_at = Instant::now();
        } else {
            let elapsed = self.updated_at.elapsed();
            if elapsed > Duration::from_secs(60) {
                const BITS_PER_HEX_DIGIT: usize = 4;
                let mut thread_manager = thread_manager.write().unwrap();
                info!(
                    "[sch_{:0width$x}]: watchdog: retire_if_stale(): stopping thread manager ({tid}/{} <= {}/{:?})...",
                    thread_manager.scheduler_id,
                    current_tick,
                    self.tick,
                    elapsed,
                    width = SchedulerId::BITS as usize / BITS_PER_HEX_DIGIT,
                );
                thread_manager.stop_threads();
                self.tick = 0;
                self.updated_at = Instant::now();
            }
        }

        true
    }
}

impl<S, TH, SEA> SchedulerPool<S, TH, SEA>
where
    S: SpawnableScheduler<TH, SEA>,
    TH: TaskHandler<SEA>,
    SEA: ScheduleExecutionArg,
{
    // Some internal impl and test code want an actual concrete type, NOT the
    // `dyn InstalledSchedulerPool`. So don't merge this into `Self::new_dyn()`.
    pub fn new(
        log_messages_bytes_limit: Option<usize>,
        transaction_status_sender: Option<TransactionStatusSender>,
        replay_vote_sender: Option<ReplayVoteSender>,
        prioritization_fee_cache: Arc<PrioritizationFeeCache>,
    ) -> Arc<Self> {
        let (scheduler_pool_sender, scheduler_pool_receiver) = bounded(1);
        let (watchdog_sender, watchdog_receiver) = unbounded();

        let watchdog_main_loop = || {
            move || {
                let scheduler_pool: Arc<Self> = scheduler_pool_receiver.recv().unwrap();
                drop(scheduler_pool_receiver);

                let mut thread_managers: Vec<WatchedThreadManager<S, TH, SEA>> = vec![];

                'outer: loop {
                    let /*mut*/ schedulers = scheduler_pool.scheduler_inners.lock().unwrap();
                    let schedulers_len_pre_retain = schedulers.len();
                    //schedulers.retain_mut(|scheduler| scheduler.retire_if_stale());
                    let schedulers_len_post_retain = schedulers.len();
                    drop(schedulers);

                    let thread_manager_len_pre_retain = thread_managers.len();
                    thread_managers.retain_mut(|thread_manager| thread_manager.retire_if_stale());

                    let thread_manager_len_pre_push = thread_managers.len();
                    'inner: loop {
                        match watchdog_receiver.try_recv() {
                            Ok(thread_manager) => {
                                thread_managers.push(WatchedThreadManager::new(thread_manager))
                            }
                            Err(TryRecvError::Disconnected) => break 'outer,
                            Err(TryRecvError::Empty) => break 'inner,
                        }
                    }

                    info!(
                        "watchdog: unused schedulers in the pool: {} => {}, all thread managers: {} => {} => {}",
                        schedulers_len_pre_retain,
                        schedulers_len_post_retain,
                        thread_manager_len_pre_retain,
                        thread_manager_len_pre_push,
                        thread_managers.len(),
                    );
                    // sleep here instead of recv_timeout() to write all logs at once.
                    sleep(Duration::from_secs(2));
                }
            }
        };

        let watchdog_thread = std::thread::Builder::new()
            .name("solScWatchdog".to_owned())
            .spawn(watchdog_main_loop())
            .unwrap();

        let scheduler_pool = Arc::new_cyclic(|weak_self| Self {
            scheduler_inners: Mutex::default(),
            handler_context: HandlerContext {
                log_messages_bytes_limit,
                transaction_status_sender,
                replay_vote_sender,
                prioritization_fee_cache,
            },
            weak_self: weak_self.clone(),
            next_scheduler_id: AtomicU64::new(PRIMARY_SCHEDULER_ID),
            _watchdog_thread: watchdog_thread,
            watchdog_sender,
        });
        scheduler_pool_sender.send(scheduler_pool.clone()).unwrap();
        scheduler_pool
    }

    // This apparently-meaningless wrapper is handy, because some callers explicitly want
    // `dyn InstalledSchedulerPool` to be returned for type inference convenience.
    pub fn new_dyn(
        log_messages_bytes_limit: Option<usize>,
        transaction_status_sender: Option<TransactionStatusSender>,
        replay_vote_sender: Option<ReplayVoteSender>,
        prioritization_fee_cache: Arc<PrioritizationFeeCache>,
    ) -> InstalledSchedulerPoolArc<SEA> {
        Self::new(
            log_messages_bytes_limit,
            transaction_status_sender,
            replay_vote_sender,
            prioritization_fee_cache,
        )
    }

    // See a comment at the weak_self field for justification of this method's existence.
    pub fn self_arc(&self) -> Arc<Self> {
        self.weak_self
            .upgrade()
            .expect("self-referencing Arc-ed pool")
    }

    fn new_scheduler_id(&self) -> SchedulerId {
        self.next_scheduler_id.fetch_add(1, Relaxed)
    }

    fn return_scheduler(&self, scheduler: S::Inner) {
        self.scheduler_inners
            .lock()
            .expect("not poisoned")
            .push(scheduler);
    }

    fn do_take_scheduler(&self, context: SchedulingContext) -> S {
        // pop is intentional for filo, expecting relatively warmed-up scheduler due to having been
        // returned recently
        if let Some(inner) = self.scheduler_inners.lock().expect("not poisoned").pop() {
            S::from_inner(inner, context)
        } else {
            S::spawn(self.self_arc(), context, TH::create(self))
        }
    }

    fn register_to_watchdog(&self, thread_manager: Weak<RwLock<ThreadManager<S, TH, SEA>>>) {
        self.watchdog_sender.send(thread_manager).unwrap();
    }
}

impl<S, TH, SEA> InstalledSchedulerPool<SEA> for SchedulerPool<S, TH, SEA>
where
    S: SpawnableScheduler<TH, SEA>,
    TH: TaskHandler<SEA>,
    SEA: ScheduleExecutionArg,
{
    fn take_scheduler(&self, context: SchedulingContext) -> Box<dyn InstalledScheduler<SEA>> {
        Box::new(self.do_take_scheduler(context))
    }
}

pub trait TaskHandler<SEA: ScheduleExecutionArg>:
    Send + Sync + Debug + Sized + Clone + 'static
{
    fn create<T: SpawnableScheduler<Self, SEA>>(pool: &SchedulerPool<T, Self, SEA>) -> Self;

    fn handle(
        &self,
        result: &mut Result<()>,
        timings: &mut ExecuteTimings,
        bank: &Arc<Bank>,
        transaction: &SanitizedTransaction,
        index: usize,
        handler_context: &HandlerContext,
    );
}

#[derive(Clone, Debug)]
pub struct DefaultTaskHandler;

impl<SEA: ScheduleExecutionArg> TaskHandler<SEA> for DefaultTaskHandler {
    fn create<T: SpawnableScheduler<Self, SEA>>(_pool: &SchedulerPool<T, Self, SEA>) -> Self {
        Self
    }

    fn handle(
        &self,
        result: &mut Result<()>,
        timings: &mut ExecuteTimings,
        bank: &Arc<Bank>,
        transaction: &SanitizedTransaction,
        index: usize,
        handler_context: &HandlerContext,
    ) {
        // scheduler must properly prevent conflicting tx executions. thus, task handler isn't
        // responsible for locking.
        let batch = bank.prepare_unlocked_batch_from_single_tx(transaction);
        let batch_with_indexes = TransactionBatchWithIndexes {
            batch,
            transaction_indexes: vec![index],
        };

        *result = execute_batch(
            &batch_with_indexes,
            bank,
            handler_context.transaction_status_sender.as_ref(),
            handler_context.replay_vote_sender.as_ref(),
            timings,
            handler_context.log_messages_bytes_limit,
            &handler_context.prioritization_fee_cache,
        );
    }
}

#[derive(Default, Debug)]
pub struct AddressBook {
    book: dashmap::DashMap<Pubkey, Page>,
}

impl AddressBook {
    pub fn load(&self, address: Pubkey) -> Page {
        self.book.entry(address).or_default().clone()
    }

    pub fn page_count(&self) -> usize {
        self.book.len()
    }

    pub fn clear(&self) {
        self.book.clear();
    }
}

#[derive(Debug)]
pub struct ExecutedTask {
    task: Task,
    result_with_timings: ResultWithTimings,
    slot: Slot,
    thx: usize,
    handler_timings: Option<HandlerTimings>,
}

#[derive(Debug)]
pub struct HandlerTimings {
    finish_time: SystemTime,
    execution_us: u64,
    execution_cpu_us: u128,
}

impl ExecutedTask {
    fn new_boxed(task: Task, thx: usize, slot: Slot) -> Box<Self> {
        Box::new(Self {
            task,
            result_with_timings: (Ok(()), ExecuteTimings::default()),
            slot,
            thx,
            handler_timings: None,
        })
    }
}

#[derive(Debug)]
pub struct PooledScheduler<TH, SEA>
where
    TH: TaskHandler<SEA>,
    SEA: ScheduleExecutionArg,
{
    inner: PooledSchedulerInner<Self, TH, SEA>,
    context: SchedulingContext,
}

#[derive(Debug)]
pub struct PooledSchedulerInner<S, TH, SEA>
where
    S: SpawnableScheduler<TH, SEA>,
    TH: TaskHandler<SEA>,
    SEA: ScheduleExecutionArg,
{
    thread_manager: Arc<RwLock<ThreadManager<S, TH, SEA>>>,
    address_book: AddressBook,
    pooled_at: Instant,
}

impl<S, TH, SEA> PooledSchedulerInner<S, TH, SEA>
where
    S: SpawnableScheduler<TH, SEA>,
    TH: TaskHandler<SEA>,
    SEA: ScheduleExecutionArg,
{
    fn pooled_since(&self) -> Duration {
        self.pooled_at.elapsed()
    }

    fn stop_thread_manager(&mut self) {
        debug!("stop_thread_manager()");
        self.thread_manager
            .write()
            .unwrap()
            .stop_threads();
    }
}

type Tid = i32;

#[derive(Debug)]
struct ThreadManager<S, TH, SEA>
where
    S: SpawnableScheduler<TH, SEA>,
    TH: TaskHandler<SEA>,
    SEA: ScheduleExecutionArg,
{
    scheduler_id: SchedulerId,
    pool: Arc<SchedulerPool<S, TH, SEA>>,
    scheduler_thread_and_tid: Option<(JoinHandle<ResultWithTimings>, Tid)>,
    handler_threads: Vec<JoinHandle<()>>,
    drop_thread: Option<JoinHandle<()>>,
    handler: TH,
    schedulrable_transaction_sender: Sender<SessionedMessage<Task, SchedulingContext>>,
    schedulable_transaction_receiver: Receiver<SessionedMessage<Task, SchedulingContext>>,
    result_sender: Sender<ResultWithTimings>,
    result_receiver: Receiver<ResultWithTimings>,
    handler_count: usize,
    session_result_with_timings: Option<ResultWithTimings>,
}

impl<TH, SEA> PooledScheduler<TH, SEA>
where
    TH: TaskHandler<SEA>,
    SEA: ScheduleExecutionArg,
{
    pub fn do_spawn(
        pool: Arc<SchedulerPool<Self, TH, SEA>>,
        initial_context: SchedulingContext,
        handler: TH,
    ) -> Self {
        let handler_count = std::env::var("SOLANA_UNIFIED_SCHEDULER_HANDLER_COUNT")
            .unwrap_or(format!("{}", 8))
            .parse::<usize>()
            .unwrap();
        let scheduler = Self::from_inner(
            PooledSchedulerInner::<Self, TH, SEA> {
                thread_manager: Arc::new(RwLock::new(ThreadManager::<Self, TH, SEA>::new(
                    handler,
                    pool.clone(),
                    handler_count,
                ))),
                address_book: AddressBook::default(),
                pooled_at: Instant::now(),
            },
            initial_context,
        );
        pool.register_to_watchdog(Arc::downgrade(&scheduler.inner.thread_manager));

        scheduler
    }

    fn ensure_thread_manager_started(
        &self,
        context: &SchedulingContext,
    ) -> RwLockReadGuard<'_, ThreadManager<Self, TH, SEA>> {
        loop {
            let r = self.inner.thread_manager.read().unwrap();
            if r.is_active() {
                debug!("ensure_thread_manager_started(): is already active...");
                return r;
            } else {
                debug!("ensure_thread_manager_started(): will start threads...");
                drop(r);
                let mut w = self.inner.thread_manager.write().unwrap();
                w.start_threads(context);
                drop(w);
            }
        }
    }
}

type ChannelAndPayload<T1, T2> = (Receiver<ChainedChannel<T1, T2>>, T2);

trait WithChannelAndPayload<T1, T2>: Send + Sync {
    fn channel_and_payload(self: Box<Self>) -> ChannelAndPayload<T1, T2>;
}

struct ChannelAndPayloadWrapper<T1, T2>(ChannelAndPayload<T1, T2>);

impl<T1: Send + Sync, T2: Send + Sync> WithChannelAndPayload<T1, T2>
    for ChannelAndPayloadWrapper<T1, T2>
{
    fn channel_and_payload(self: Box<Self>) -> ChannelAndPayload<T1, T2> {
        self.0
    }
}

enum ChainedChannel<T1, T2> {
    Payload(T1),
    ChannelWithPayload(Box<dyn WithChannelAndPayload<T1, T2>>),
}

enum SessionedMessage<T1, T2> {
    Payload(T1),
    StartSession(T2),
    EndSession,
}

impl<T1: Send + Sync + 'static, T2: Send + Sync + 'static> ChainedChannel<T1, T2> {
    fn new_channel(receiver: Receiver<Self>, payload: T2) -> Self {
        Self::ChannelWithPayload(Box::new(ChannelAndPayloadWrapper((receiver, payload))))
    }
}

#[derive(Default)]
struct LogInterval(usize);

impl LogInterval {
    fn increment(&mut self) -> bool {
        let should_log = self.0 % 1000 == 0;
        self.0 += 1;
        should_log
    }
}

const PRIMARY_SCHEDULER_ID: SchedulerId = 0;

impl<S, TH, SEA> ThreadManager<S, TH, SEA>
where
    S: SpawnableScheduler<TH, SEA>,
    TH: TaskHandler<SEA>,
    SEA: ScheduleExecutionArg,
{
    fn new(handler: TH, pool: Arc<SchedulerPool<S, TH, SEA>>, handler_count: usize) -> Self {
        let (schedulrable_transaction_sender, schedulable_transaction_receiver) = unbounded();
        let (result_sender, result_receiver) = unbounded();

        Self {
            scheduler_id: pool.new_scheduler_id(),
            schedulrable_transaction_sender,
            schedulable_transaction_receiver,
            result_sender,
            result_receiver,
            scheduler_thread_and_tid: None,
            drop_thread: None,
            handler_threads: Vec::with_capacity(handler_count),
            handler_count,
            handler,
            pool,
            session_result_with_timings: Some((Ok(()), ExecuteTimings::default())),
        }
    }

    fn is_active(&self) -> bool {
        self.scheduler_thread_and_tid.is_some()
    }

    fn receive_scheduled_transaction(
        handler: &TH,
        bank: &Arc<Bank>,
        task: &mut Box<ExecutedTask>,
        pool: &Arc<SchedulerPool<S, TH, SEA>>,
        send_metrics: bool,
    ) {
        use solana_measure::measure::Measure;
        let handler_timings = send_metrics.then_some((
            Measure::start("process_message_time"),
            cpu_time::ThreadTime::now(),
        ));
        debug!("handling task at {:?}", std::thread::current());
        TH::handle(
            handler,
            &mut task.result_with_timings.0,
            &mut task.result_with_timings.1,
            bank,
            task.task.transaction(),
            task.task.task_index(),
            &pool.handler_context,
        );
        if let Some((mut wall_time, cpu_time)) = handler_timings {
            task.handler_timings = Some(HandlerTimings {
                finish_time: SystemTime::now(),
                execution_cpu_us: cpu_time.elapsed().as_micros(),
                execution_us: {
                    // make wall time is longer than cpu time, always
                    wall_time.stop();
                    wall_time.as_us()
                },
            });
        }
    }

    fn propagate_context(
        blocked_transaction_sessioned_sender: &mut Sender<ChainedChannel<Task, SchedulingContext>>,
        context: SchedulingContext,
        handler_count: usize,
    ) {
        let (next_blocked_transaction_sessioned_sender, blocked_transaction_sessioned_receiver) =
            unbounded();
        for _ in 0..handler_count {
            blocked_transaction_sessioned_sender
                .send(ChainedChannel::new_channel(
                    blocked_transaction_sessioned_receiver.clone(),
                    context.clone(),
                ))
                .unwrap();
        }
        *blocked_transaction_sessioned_sender = next_blocked_transaction_sessioned_sender;
    }

    fn take_session_result_with_timings(&mut self) -> ResultWithTimings {
        self.session_result_with_timings.take().unwrap()
    }

    fn put_session_result_with_timings(&mut self, result_with_timings: ResultWithTimings) {
        assert_matches!(self.session_result_with_timings.replace(result_with_timings), None);
    }

    fn start_threads(
        &mut self,
        context: &SchedulingContext,
    ) {
        if self.is_active() {
            // this can't be promoted to panic! as read => write upgrade isn't completely
            // race-free in ensure_thread_manager_started()...
            warn!("start_threads(): already started");
            return;
        }
        debug!("start_threads(): doing now");

        let send_metrics = std::env::var("SOLANA_TRANSACTION_TIMINGS").is_ok();

        let (blocked_transaction_sessioned_sender, blocked_transaction_sessioned_receiver) =
            unbounded::<ChainedChannel<Task, SchedulingContext>>();
        let (idle_transaction_sender, idle_transaction_receiver) = unbounded::<Task>();
        let (handled_blocked_transaction_sender, handled_blocked_transaction_receiver) =
            unbounded::<Box<ExecutedTask>>();
        let (handled_idle_transaction_sender, handled_idle_transaction_receiver) =
            unbounded::<Box<ExecutedTask>>();
        let (drop_sender, drop_receiver) =
            unbounded::<SessionedMessage<Box<ExecutedTask>, ResultWithTimings>>();
        let (drop_sender2, drop_receiver2) = unbounded::<ResultWithTimings>();
        let scheduler_id = self.scheduler_id;
        let mut slot = context.bank().slot();
        let (tid_sender, tid_receiver) = bounded(1);
        let result_with_timings = self.take_session_result_with_timings();

        let scheduler_main_loop = || {
            let handler_count = self.handler_count;
            let result_sender = self.result_sender.clone();
            let mut schedulable_transaction_receiver =
                self.schedulable_transaction_receiver.clone();
            let mut blocked_transaction_sessioned_sender =
                blocked_transaction_sessioned_sender.clone();
            drop_sender
                .send(SessionedMessage::StartSession(result_with_timings))
                .unwrap();

            let mut session_ending = false;
            let mut thread_ending = false;
            move || {
                let mut state_machine = SchedulingStateMachine::default();
                let mut log_interval = LogInterval::default();
                // hint compiler about inline[never] and unlikely?
                macro_rules! log_scheduler {
                    ($prefix:tt) => {
                        const BITS_PER_HEX_DIGIT: usize = 4;
                        info!(
                            "[sch_{:0width$x}]: slot: {}[{:8}]({}/{}): state_machine(({}(+{})=>{})/{}|{}/{}) channels(<{} >{}+{} <{}+{})",
                            scheduler_id, slot,
                            (if ($prefix) == "step" { "interval" } else { $prefix }),
                            (if thread_ending {"T"} else {"-"}), (if session_ending {"S"} else {"-"}),
                            state_machine.active_task_count(), state_machine.retryable_task_count(), state_machine.handled_task_count(),
                            state_machine.total_task_count(),
                            state_machine.reschedule_count(),
                            state_machine.rescheduled_task_count(),
                            schedulable_transaction_receiver.len(),
                            blocked_transaction_sessioned_sender.len(), idle_transaction_sender.len(),
                            handled_blocked_transaction_receiver.len(), handled_idle_transaction_receiver.len(),
                            width = SchedulerId::BITS as usize / BITS_PER_HEX_DIGIT,
                        );
                    };
                }

                trace!(
                    "solScheduler thread is started at: {:?}",
                    std::thread::current()
                );
                tid_sender
                    .send(rustix::thread::gettid().as_raw_nonzero().get())
                    .unwrap();
                let (always_retry, never_retry) = (&disconnected::<()>(), &never());

                while !thread_ending {
                    loop {
                        let state_change = select_biased! {
                            recv(handled_blocked_transaction_receiver) -> task => {
                                let task = task.unwrap();
                                state_machine.deschedule_task(&task.task);
                                drop_sender.send_buffered(SessionedMessage::Payload(task)).unwrap();
                                "step"
                            },
                            recv(schedulable_transaction_receiver) -> m => {
                                match m {
                                    Ok(SessionedMessage::Payload(payload)) => {
                                        assert!(!session_ending && !thread_ending);
                                        if let Some(task) = state_machine.schedule_new_task(payload) {
                                            idle_transaction_sender.send(task).unwrap();
                                        }
                                        "step"
                                    }
                                    Ok(SessionedMessage::StartSession(context)) => {
                                        slot = context.bank().slot();
                                        Self::propagate_context(&mut blocked_transaction_sessioned_sender, context, handler_count);
                                        "started"
                                    }
                                    Ok(SessionedMessage::EndSession) => {
                                        assert!(!session_ending && !thread_ending);
                                        session_ending = true;
                                        "S:ending"
                                    }
                                    Err(_) => {
                                        assert!(!thread_ending);
                                        thread_ending = true;
                                        schedulable_transaction_receiver = never();
                                        "T:ending"
                                    }
                                }
                            },
                            recv(if state_machine.has_retryable_task() { always_retry } else { never_retry }) -> result => {
                                assert_matches!(result, Err(RecvError));

                                if let Some(task) = state_machine.schedule_retryable_task() {
                                    blocked_transaction_sessioned_sender
                                        .send(ChainedChannel::Payload(task))
                                        .unwrap();
                                }
                                "step"
                            },
                            recv(handled_idle_transaction_receiver) -> task => {
                                let task = task.unwrap();
                                state_machine.deschedule_task(&task.task);
                                drop_sender.send_buffered(SessionedMessage::Payload(task)).unwrap();
                                "step"
                            },
                        };
                        if state_change != "step" || log_interval.increment() {
                            log_scheduler!(state_change);
                        }

                        let is_finished =
                            state_machine.is_empty() && (session_ending || thread_ending);
                        if is_finished {
                            break;
                        }
                    }

                    if session_ending {
                        log_scheduler!("S:ended");
                        (state_machine, log_interval) = <_>::default();
                        drop_sender.send(SessionedMessage::EndSession).unwrap();
                        result_sender.send(drop_receiver2.recv().unwrap()).unwrap();
                        if !thread_ending {
                            session_ending = false;
                        }
                    }
                }

                log_scheduler!("T:ended");
                let result_with_timings = if session_ending {
                    (Ok(()), ExecuteTimings::default())
                } else {
                    drop_sender.send(SessionedMessage::EndSession).unwrap();
                    drop_receiver2.recv().unwrap()
                };
                trace!(
                    "solScheduler thread is ended at: {:?}",
                    std::thread::current()
                );
                result_with_timings
            }
        };

        let handler_main_loop = |thx| {
            let pool = self.pool.clone();
            let handler = self.handler.clone();
            let mut bank = context.bank().clone();
            let mut blocked_transaction_sessioned_receiver =
                blocked_transaction_sessioned_receiver.clone();
            let mut idle_transaction_receiver = idle_transaction_receiver.clone();
            let handled_blocked_transaction_sender = handled_blocked_transaction_sender.clone();
            let handled_idle_transaction_sender = handled_idle_transaction_sender.clone();

            move || {
                trace!(
                    "solScHandler{:02} thread is started at: {:?}",
                    thx,
                    std::thread::current()
                );
                loop {
                    let (task, sender) = select_biased! {
                        recv(blocked_transaction_sessioned_receiver) -> message => {
                            match message {
                                Ok(ChainedChannel::Payload(task)) => {
                                    (task, &handled_blocked_transaction_sender)
                                }
                                Ok(ChainedChannel::ChannelWithPayload(new_channel)) => {
                                    let new_context;
                                    (blocked_transaction_sessioned_receiver, new_context) = new_channel.channel_and_payload();
                                    bank = new_context.bank().clone();
                                    continue;
                                }
                                Err(_) => break,
                            }
                        },
                        recv(idle_transaction_receiver) -> task => {
                            if let Ok(task) = task {
                                (task, &handled_idle_transaction_sender)
                            } else {
                                idle_transaction_receiver = never();
                                continue;
                            }
                        },
                    };
                    let mut task = ExecutedTask::new_boxed(task, thx, bank.slot());
                    Self::receive_scheduled_transaction(
                        &handler,
                        &bank,
                        &mut task,
                        &pool,
                        send_metrics,
                    );
                    sender.send(task).unwrap();
                }
                trace!(
                    "solScHandler{:02} thread is ended at: {:?}",
                    thx,
                    std::thread::current()
                );
            }
        };

        let drop_main_loop = || {
            move || 'outer: {
                let mut session_result: Result<()> = Ok(());
                let mut session_timings = ExecuteTimings::default();
                loop {
                    match drop_receiver.recv_timeout(Duration::from_millis(40)) {
                        Ok(SessionedMessage::Payload(task)) => {
                            session_timings.accumulate(&task.result_with_timings.1);
                            match &task.result_with_timings.0 {
                                Ok(()) => {}
                                Err(e) => session_result = Err(e.clone()),
                            }
                            if let Some(handler_timings) = &task.handler_timings {
                                use solana_runtime::transaction_priority_details::GetTransactionPriorityDetails;

                                let sig = task.task.transaction().signature().to_string();

                                solana_metrics::datapoint_info_at!(
                                    handler_timings.finish_time,
                                    "transaction_timings",
                                    ("slot", task.slot, i64),
                                    ("index", task.task.task_index(), i64),
                                    ("thread", format!("solScExLane{:02}", task.thx), String),
                                    ("signature", &sig, String),
                                    (
                                        "account_locks_in_json",
                                        serde_json::to_string(
                                            &task.task.transaction().get_account_locks_unchecked()
                                        )
                                        .unwrap(),
                                        String
                                    ),
                                    (
                                        "status",
                                        format!("{:?}", task.result_with_timings.0),
                                        String
                                    ),
                                    ("duration", handler_timings.execution_us, i64),
                                    ("cpu_duration", handler_timings.execution_cpu_us, i64),
                                    ("compute_units", 0 /*task.cu*/, i64),
                                    (
                                        "priority",
                                        task.task
                                            .transaction()
                                            .get_transaction_priority_details(false)
                                            .map(|d| d.priority)
                                            .unwrap_or_default(),
                                        i64
                                    ),
                                );
                            }
                            drop(task);
                        }
                        Ok(SessionedMessage::StartSession(result_with_timings)) => {
                            (session_result, session_timings) = result_with_timings;
                        }
                        Ok(SessionedMessage::EndSession) => {
                            drop_sender2
                                .send((session_result, session_timings))
                                .unwrap();
                            session_result = Ok(());
                            session_timings = ExecuteTimings::default();
                        }
                        Err(RecvTimeoutError::Disconnected) => break 'outer,
                        Err(RecvTimeoutError::Timeout) => continue,
                    }
                }
            }
        };

        self.scheduler_thread_and_tid = Some((
            std::thread::Builder::new()
                .name("solScheduler".to_owned())
                .spawn(scheduler_main_loop())
                .unwrap(),
            tid_receiver.recv().unwrap(),
        ));

        self.drop_thread = Some(
            std::thread::Builder::new()
                .name("solScDrop".to_owned())
                .spawn(drop_main_loop())
                .unwrap(),
        );

        self.handler_threads = (0..self.handler_count)
            .map({
                |thx| {
                    std::thread::Builder::new()
                        .name(format!("solScHandler{:02}", thx))
                        .spawn(handler_main_loop(thx))
                        .unwrap()
                }
            })
            .collect();
    }

    fn stop_threads(&mut self) {
        if !self.is_active() {
            warn!("stop_threads(): already not active anymore...");
            return;
        }
        debug!(
            "stop_threads(): stopping threads by {:?}",
            std::thread::current()
        );

        (
            self.schedulrable_transaction_sender,
            self.schedulable_transaction_receiver,
        ) = unbounded();
        let r = self
            .scheduler_thread_and_tid
            .take()
            .unwrap()
            .0
            .join()
            .unwrap();
        self.put_session_result_with_timings(r);
        let () = self.drop_thread.take().unwrap().join().unwrap();

        for j in self.handler_threads.drain(..) {
            debug!("joining...: {:?}", j);
            () = j.join().unwrap();
        }
        debug!(
            "stop_threads(): successfully stopped threads by {:?}",
            std::thread::current()
        );
    }

    fn send_task(&self, task: Task) {
        debug!("send_task()");
        self.schedulrable_transaction_sender
            .send(SessionedMessage::Payload(task))
            .unwrap();
    }

    fn end_session(
        &mut self,
    ) {
        debug!("end_session(): will end session...");
        if !self.is_active() {
            assert_matches!(self.session_result_with_timings, Some(_));
            return;
        }

        self.schedulrable_transaction_sender
            .send(SessionedMessage::EndSession)
            .unwrap();
        self.put_session_result_with_timings(self.result_receiver.recv().unwrap());
    }

    fn start_session(
        &mut self,
        context: &SchedulingContext,
    ) {
        if self.is_active() {
            self.schedulrable_transaction_sender
                .send(SessionedMessage::StartSession(context.clone()))
                .unwrap();
        } else {
            self.start_threads(context);
        }
    }

    fn is_primary(&self) -> bool {
        self.scheduler_id == PRIMARY_SCHEDULER_ID
    }

    fn active_tid_if_not_primary(&self) -> Option<Tid> {
        if self.is_primary() {
            // always exempt from watchdog...
            None
        } else {
            self.scheduler_thread_and_tid.as_ref().map(|&(_, tid)| tid)
        }
    }
}

pub trait SpawnableScheduler<TH, SEA>: InstalledScheduler<SEA>
where
    TH: TaskHandler<SEA>,
    SEA: ScheduleExecutionArg,
{
    type Inner: Debug + Send + Sync + AAA;

    fn into_inner(self) -> (ResultWithTimings, Self::Inner);

    fn from_inner(inner: Self::Inner, context: SchedulingContext) -> Self;

    fn spawn(
        pool: Arc<SchedulerPool<Self, TH, SEA>>,
        initial_context: SchedulingContext,
        handler: TH,
    ) -> Self
    where
        Self: Sized;

    /*
    fn retire_if_stale(&mut self) -> bool
    where
        Self: Sized;
    */
}

pub trait AAA {
}

impl<TH, SEA> SpawnableScheduler<TH, SEA> for PooledScheduler<TH, SEA>
where
    TH: TaskHandler<SEA>,
    SEA: ScheduleExecutionArg,
{
    type Inner = PooledSchedulerInner<Self, TH, SEA>;

    fn into_inner(self) -> (ResultWithTimings, Self::Inner) {
        self.inner
            .thread_manager
            .write()
            .unwrap()
            .end_session();
        let r = self.inner.thread_manager.write().unwrap().take_session_result_with_timings();
        (r, self.inner)
    }

    fn from_inner(inner: Self::Inner, context: SchedulingContext) -> Self {
        inner
            .thread_manager
            .write()
            .unwrap()
            .start_session(&context);

        Self {
            inner,
            context,
        }
    }

    fn spawn(
        pool: Arc<SchedulerPool<Self, TH, SEA>>,
        initial_context: SchedulingContext,
        handler: TH,
    ) -> Self {
        Self::do_spawn(pool, initial_context, handler)
    }

    /*
    fn retire_if_stale(&mut self) -> bool {
        const BITS_PER_HEX_DIGIT: usize = 4;
        let page_count = self.inner.address_book.page_count();
        if page_count < 200_000 {
            info!(
                "[sch_{:0width$x}]: watchdog: address book size: {page_count}...",
                self.id(),
                width = SchedulerId::BITS as usize / BITS_PER_HEX_DIGIT,
            );
        } else if self.inner.thread_manager.read().unwrap().is_primary() {
            info!(
                "[sch_{:0width$x}]: watchdog: too big address book size: {page_count}...; emptying the primary scheduler",
                self.id(),
                width = SchedulerId::BITS as usize / BITS_PER_HEX_DIGIT,
            );
            self.inner.address_book.clear();
            return true;
        } else {
            info!(
                "[sch_{:0width$x}]: watchdog: too big address book size: {page_count}...; retiring scheduler",
                self.id(),
                width = SchedulerId::BITS as usize / BITS_PER_HEX_DIGIT,
            );
            self.stop_thread_manager();
            return false;
        }

        let pooled_duration = self.pooled_since();
        if pooled_duration <= Duration::from_secs(600) {
            true
        } else if !self.inner.thread_manager.read().unwrap().is_primary() {
            info!(
                "[sch_{:0width$x}]: watchdog: retiring unused scheduler after {:?}...",
                self.id(),
                pooled_duration,
                width = SchedulerId::BITS as usize / BITS_PER_HEX_DIGIT,
            );
            self.stop_thread_manager();
            false
        } else {
            true
        }
    }
    */
}

impl<TH, SEA> InstalledScheduler<SEA> for PooledScheduler<TH, SEA>
where
    TH: TaskHandler<SEA>,
    SEA: ScheduleExecutionArg,
{
    fn id(&self) -> SchedulerId {
        self.inner.thread_manager.read().unwrap().scheduler_id
    }

    fn context(&self) -> &SchedulingContext {
        &self.context
    }

    fn schedule_execution(&self, transaction_with_index: SEA::TransactionWithIndex<'_>) {
        transaction_with_index.with_transaction_and_index(|transaction, index| {
            let task = SchedulingStateMachine::create_task(transaction.clone(), index, |pubkey| {
                self.inner.address_book.load(pubkey)
            });
            self.ensure_thread_manager_started(&self.context)
                .send_task(task);
        });
    }

    fn wait_for_termination(
        self: Box<Self>,
        _is_dropped: bool,
    ) -> (ResultWithTimings, UninstalledSchedulerBox) {
        let (result_with_timings, uninstalled_scheduler) = self.into_inner();
        (result_with_timings, Box::new(uninstalled_scheduler))
    }

    fn pause_for_recent_blockhash(&mut self) {
        self.inner
            .thread_manager
            .write()
            .unwrap()
            .end_session();
    }
}

impl<S, TH, SEA> UninstalledScheduler for PooledSchedulerInner<S, TH, SEA>
where
    S: SpawnableScheduler<TH, SEA, Inner = PooledSchedulerInner<S, TH, SEA>>,
    TH: TaskHandler<SEA>,
    SEA: ScheduleExecutionArg,
{
    fn return_to_pool(mut self: Box<Self>) {
        let pool = self.thread_manager.read().unwrap().pool.clone();
        self.pooled_at = Instant::now();
        pool.return_scheduler(*self)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        solana_runtime::{
            bank::Bank,
            bank_forks::BankForks,
            genesis_utils::{create_genesis_config, GenesisConfigInfo},
            installed_scheduler_pool::{BankWithScheduler, SchedulingContext},
            prioritization_fee_cache::PrioritizationFeeCache,
        },
        solana_sdk::{
            clock::MAX_PROCESSING_AGE,
            pubkey::Pubkey,
            scheduling::SchedulingMode,
            signer::keypair::Keypair,
            system_transaction,
            transaction::{SanitizedTransaction, TransactionError},
        },
        std::{sync::Arc, thread::JoinHandle},
    };

    #[test]
    fn test_scheduler_pool_new() {
        solana_logger::setup();

        let ignored_prioritization_fee_cache = Arc::new(PrioritizationFeeCache::new(0u64));
        let pool =
            DefaultSchedulerPool::new_dyn(None, None, None, ignored_prioritization_fee_cache);

        // this indirectly proves that there should be circular link because there's only one Arc
        // at this moment now
        assert_eq!((Arc::strong_count(&pool), Arc::weak_count(&pool)), (1, 1));
        let debug = format!("{pool:#?}");
        assert!(!debug.is_empty());
    }

    #[test]
    fn test_scheduler_spawn() {
        solana_logger::setup();

        let ignored_prioritization_fee_cache = Arc::new(PrioritizationFeeCache::new(0u64));
        let pool =
            DefaultSchedulerPool::new_dyn(None, None, None, ignored_prioritization_fee_cache);
        let bank = Arc::new(Bank::default_for_tests());
        let context = SchedulingContext::new(SchedulingMode::BlockVerification, bank);
        let scheduler = pool.take_scheduler(context);

        let debug = format!("{scheduler:#?}");
        assert!(!debug.is_empty());
    }

    #[test]
    fn test_scheduler_pool_filo() {
        solana_logger::setup();

        let ignored_prioritization_fee_cache = Arc::new(PrioritizationFeeCache::new(0u64));
        let pool = DefaultSchedulerPool::new(None, None, None, ignored_prioritization_fee_cache);
        let bank = Arc::new(Bank::default_for_tests());
        let context = &SchedulingContext::new(SchedulingMode::BlockVerification, bank);

        let scheduler1 = pool.do_take_scheduler(context.clone());
        let scheduler_id1 = scheduler1.id();
        let scheduler2 = pool.do_take_scheduler(context.clone());
        let scheduler_id2 = scheduler2.id();
        assert_ne!(scheduler_id1, scheduler_id2);

        let (result_with_timings, scheduler1) = scheduler1.into_inner();
        assert_matches!(result_with_timings, (Ok(()), _));
        pool.return_scheduler(scheduler1);
        let (result_with_timings, scheduler2) = scheduler2.into_inner();
        assert_matches!(result_with_timings, (Ok(()), _));
        pool.return_scheduler(scheduler2);

        let scheduler3 = pool.do_take_scheduler(context.clone());
        assert_eq!(scheduler_id2, scheduler3.id());
        let scheduler4 = pool.do_take_scheduler(context.clone());
        assert_eq!(scheduler_id1, scheduler4.id());
    }

    #[test]
    fn test_scheduler_pool_context_drop_unless_reinitialized() {
        solana_logger::setup();

        let ignored_prioritization_fee_cache = Arc::new(PrioritizationFeeCache::new(0u64));
        let pool = DefaultSchedulerPool::new(None, None, None, ignored_prioritization_fee_cache);
        let bank = Arc::new(Bank::default_for_tests());
        let context = &SchedulingContext::new(SchedulingMode::BlockVerification, bank);
        let mut scheduler = pool.do_take_scheduler(context.clone());

        // should never panic.
        scheduler.pause_for_recent_blockhash();
        assert_matches!(
            Box::new(scheduler).wait_for_termination(false),
            ((Ok(()), _), _)
        );
    }

    #[test]
    fn test_scheduler_pool_context_replace() {
        solana_logger::setup();

        let ignored_prioritization_fee_cache = Arc::new(PrioritizationFeeCache::new(0u64));
        let pool = DefaultSchedulerPool::new(None, None, None, ignored_prioritization_fee_cache);
        let old_bank = &Arc::new(Bank::default_for_tests());
        let new_bank = &Arc::new(Bank::default_for_tests());
        assert!(!Arc::ptr_eq(old_bank, new_bank));

        let old_context =
            &SchedulingContext::new(SchedulingMode::BlockVerification, old_bank.clone());
        let new_context =
            &SchedulingContext::new(SchedulingMode::BlockVerification, new_bank.clone());

        let scheduler = pool.do_take_scheduler(old_context.clone());
        let scheduler_id = scheduler.id();
        pool.return_scheduler(scheduler.into_inner().1);

        let scheduler = pool.take_scheduler(new_context.clone());
        assert_eq!(scheduler_id, scheduler.id());
        assert!(Arc::ptr_eq(scheduler.context().bank(), new_bank));
    }

    #[test]
    fn test_scheduler_pool_install_into_bank_forks() {
        solana_logger::setup();

        let bank = Bank::default_for_tests();
        let bank_forks = BankForks::new_rw_arc(bank);
        let mut bank_forks = bank_forks.write().unwrap();
        let ignored_prioritization_fee_cache = Arc::new(PrioritizationFeeCache::new(0u64));
        let pool =
            DefaultSchedulerPool::new_dyn(None, None, None, ignored_prioritization_fee_cache);
        bank_forks.install_scheduler_pool(pool);
    }

    #[test]
    fn test_scheduler_install_into_bank() {
        solana_logger::setup();

        let GenesisConfigInfo { genesis_config, .. } = create_genesis_config(10_000);
        let bank = Arc::new(Bank::new_for_tests(&genesis_config));
        let child_bank = Bank::new_from_parent(bank, &Pubkey::default(), 1);

        let ignored_prioritization_fee_cache = Arc::new(PrioritizationFeeCache::new(0u64));
        let pool =
            DefaultSchedulerPool::new_dyn(None, None, None, ignored_prioritization_fee_cache);

        let bank = Bank::default_for_tests();
        let bank_forks = BankForks::new_rw_arc(bank);
        let mut bank_forks = bank_forks.write().unwrap();

        // existing banks in bank_forks shouldn't process transactions anymore in general, so
        // shouldn't be touched
        assert!(!bank_forks
            .working_bank_with_scheduler()
            .has_installed_scheduler());
        bank_forks.install_scheduler_pool(pool);
        assert!(!bank_forks
            .working_bank_with_scheduler()
            .has_installed_scheduler());

        let mut child_bank = bank_forks.insert(child_bank);
        assert!(child_bank.has_installed_scheduler());
        bank_forks.remove(child_bank.slot());
        child_bank.drop_scheduler();
        assert!(!child_bank.has_installed_scheduler());
    }

    fn setup_dummy_fork_graph(bank: Bank) -> Arc<Bank> {
        let slot = bank.slot();
        let bank_fork = BankForks::new_rw_arc(bank);
        let bank = bank_fork.read().unwrap().get(slot).unwrap();
        bank.loaded_programs_cache
            .write()
            .unwrap()
            .set_fork_graph(bank_fork);
        bank
    }

    #[test]
    fn test_scheduler_schedule_execution_success() {
        solana_logger::setup();

        let GenesisConfigInfo {
            genesis_config,
            mint_keypair,
            ..
        } = create_genesis_config(10_000);
        let tx0 = &SanitizedTransaction::from_transaction_for_tests(system_transaction::transfer(
            &mint_keypair,
            &solana_sdk::pubkey::new_rand(),
            2,
            genesis_config.hash(),
        ));
        let bank = Bank::new_for_tests(&genesis_config);
        let bank = setup_dummy_fork_graph(bank);
        let ignored_prioritization_fee_cache = Arc::new(PrioritizationFeeCache::new(0u64));
        let pool =
            DefaultSchedulerPool::new_dyn(None, None, None, ignored_prioritization_fee_cache);
        let context = SchedulingContext::new(SchedulingMode::BlockVerification, bank.clone());

        assert_eq!(bank.transaction_count(), 0);
        let scheduler = pool.take_scheduler(context);
        scheduler.schedule_execution(&(tx0, 0));
        let bank = BankWithScheduler::new(bank, Some(scheduler));
        assert_matches!(bank.wait_for_completed_scheduler(), Some((Ok(()), _)));
        assert_eq!(bank.transaction_count(), 1);
    }

    #[test]
    fn test_scheduler_schedule_execution_failure() {
        solana_logger::setup();

        let GenesisConfigInfo {
            genesis_config,
            mint_keypair,
            ..
        } = create_genesis_config(10_000);
        let bank = Bank::new_for_tests(&genesis_config);
        let bank = setup_dummy_fork_graph(bank);

        let ignored_prioritization_fee_cache = Arc::new(PrioritizationFeeCache::new(0u64));
        let pool =
            DefaultSchedulerPool::new_dyn(None, None, None, ignored_prioritization_fee_cache);
        let context = SchedulingContext::new(SchedulingMode::BlockVerification, bank.clone());
        let mut scheduler = pool.take_scheduler(context);

        let unfunded_keypair = Keypair::new();
        let bad_tx =
            &SanitizedTransaction::from_transaction_for_tests(system_transaction::transfer(
                &unfunded_keypair,
                &solana_sdk::pubkey::new_rand(),
                2,
                genesis_config.hash(),
            ));
        assert_eq!(bank.transaction_count(), 0);
        scheduler.schedule_execution(&(bad_tx, 0));
        scheduler.pause_for_recent_blockhash();
        assert_eq!(bank.transaction_count(), 0);

        let good_tx_after_bad_tx =
            &SanitizedTransaction::from_transaction_for_tests(system_transaction::transfer(
                &mint_keypair,
                &solana_sdk::pubkey::new_rand(),
                3,
                genesis_config.hash(),
            ));
        // make sure this tx is really a good one to execute.
        assert_matches!(
            bank.simulate_transaction_unchecked(good_tx_after_bad_tx, false)
                .result,
            Ok(_)
        );
        scheduler.schedule_execution(&(good_tx_after_bad_tx, 0));
        scheduler.pause_for_recent_blockhash();
        // transaction_count should remain same as scheduler should be bailing out.
        assert_eq!(bank.transaction_count(), 0);

        let bank = BankWithScheduler::new(bank, Some(scheduler));
        assert_matches!(
            bank.wait_for_completed_scheduler(),
            Some((
                Err(solana_sdk::transaction::TransactionError::AccountNotFound),
                _timings
            ))
        );
    }

    #[derive(Debug)]
    struct AsyncScheduler<const TRIGGER_RACE_CONDITION: bool>(
        PooledScheduler<DefaultTaskHandler, DefaultScheduleExecutionArg>,
        Mutex<Vec<JoinHandle<ResultWithTimings>>>,
    );

    impl<const TRIGGER_RACE_CONDITION: bool> AsyncScheduler<TRIGGER_RACE_CONDITION> {
        fn do_wait(&self) {
            let mut overall_result = Ok(());
            let mut overall_timings = ExecuteTimings::default();
            for handle in self.1.lock().unwrap().drain(..) {
                let (result, timings) = handle.join().unwrap();
                match result {
                    Ok(()) => {}
                    Err(e) => overall_result = Err(e),
                }
                overall_timings.accumulate(&timings);
            }
            *self.0.result_with_timings.lock().unwrap() = (overall_result, overall_timings);
        }
    }

    impl<const TRIGGER_RACE_CONDITION: bool> InstalledScheduler<DefaultScheduleExecutionArg>
        for AsyncScheduler<TRIGGER_RACE_CONDITION>
    {
        fn id(&self) -> SchedulerId {
            self.0.id()
        }

        fn context(&self) -> &SchedulingContext {
            self.0.context()
        }

        fn schedule_execution<'a>(
            &'a self,
            &(_transaction, _index): <DefaultScheduleExecutionArg as ScheduleExecutionArg>::TransactionWithIndex<'a>,
        ) {
            todo!();
            /*
            let transaction_and_index = (transaction.clone(), index);
            let context = self.context().clone();
            let pool = self.0.pool.clone();

            self.1.lock().unwrap().push(std::thread::spawn(move || {
                // intentionally sleep to simulate race condition where register_recent_blockhash
                // is run before finishing executing scheduled transactions
                std::thread::sleep(std::time::Duration::from_secs(1));

                let mut result = Ok(());
                let mut timings = ExecuteTimings::default();

                <DefaultTaskHandler as TaskHandler<DefaultScheduleExecutionArg>>::handle(
                    &DefaultTaskHandler,
                    &mut result,
                    &mut timings,
                    context.bank(),
                    &transaction_and_index.0,
                    transaction_and_index.1,
                    &pool,
                );
                (result, timings)
            }));
            */
        }

        fn wait_for_termination(&mut self, _reason: &WaitReason) -> Option<ResultWithTimings> {
            todo!();
            /*
            if TRIGGER_RACE_CONDITION && matches!(reason, WaitReason::PausedForRecentBlockhash) {
                // this is equivalent to NOT calling wait_for_paused_scheduler() in
                // register_recent_blockhash().
                return None;
            }

            let mut overall_result = Ok(());
            let mut overall_timings = ExecuteTimings::default();
            for handle in self.1.lock().unwrap().drain(..) {
                let (result, timings) = handle.join().unwrap();
                match result {
                    Ok(()) => {}
                    Err(e) => overall_result = Err(e),
                }
                overall_timings.accumulate(&timings);
            }
            *self.0.result_with_timings.lock().unwrap() = Some((overall_result, overall_timings));

            self.0.wait_for_termination(reason)
            */
        }

        fn return_to_pool(self: Box<Self>) {
            Box::new(self.0).return_to_pool()
        }
    }

    impl<const TRIGGER_RACE_CONDITION: bool>
        SpawnableScheduler<DefaultTaskHandler, DefaultScheduleExecutionArg>
        for AsyncScheduler<TRIGGER_RACE_CONDITION>
    {
        // well, i wish i can use ! (never type).....
        type Inner = Self;

        fn into_inner(self) -> (ResultWithTimings, Self::Inner) {
            todo!();
        }

        fn from_inner(_inner: Self::Inner, _context: SchedulingContext) -> Self {
            todo!();
        }

        fn spawn(
            _pool: Arc<SchedulerPool<Self, DefaultTaskHandler, DefaultScheduleExecutionArg>>,
            _initial_context: SchedulingContext,
            _handler: DefaultTaskHandler,
        ) -> Self {
            todo!();
            /*
            AsyncScheduler::<TRIGGER_RACE_CONDITION>(
                PooledScheduler::<DefaultTaskHandler, DefaultScheduleExecutionArg> {
                    id: thread_rng().gen::<SchedulerId>(),
                    pool: SchedulerPool::new(
                        pool.log_messages_bytes_limit,
                        pool.transaction_status_sender.clone(),
                        pool.replay_vote_sender.clone(),
                        pool.prioritization_fee_cache.clone(),
                    ),
                    context: Some(initial_context),
                    result_with_timings: Mutex::default(),
                    handler,
                    _phantom: PhantomData,
                },
                Mutex::new(vec![]),
            )
            */
        }

        /*
        fn retire_if_stale(&mut self) -> bool {
            todo!();
        }
        */
    }

    fn do_test_scheduler_schedule_execution_recent_blockhash_edge_case<
        const TRIGGER_RACE_CONDITION: bool,
    >() {
        solana_logger::setup();

        let GenesisConfigInfo {
            genesis_config,
            mint_keypair,
            ..
        } = create_genesis_config(10_000);
        let very_old_valid_tx =
            SanitizedTransaction::from_transaction_for_tests(system_transaction::transfer(
                &mint_keypair,
                &solana_sdk::pubkey::new_rand(),
                2,
                genesis_config.hash(),
            ));
        let mut bank = Bank::new_for_tests(&genesis_config);
        for _ in 0..MAX_PROCESSING_AGE {
            bank.fill_bank_with_ticks_for_tests();
            bank.freeze();
            let slot = bank.slot();
            bank = Bank::new_from_parent(
                Arc::new(bank),
                &Pubkey::default(),
                slot.checked_add(1).unwrap(),
            );
        }
        let bank = setup_dummy_fork_graph(bank);
        let context = SchedulingContext::new(SchedulingMode::BlockVerification, bank.clone());

        let ignored_prioritization_fee_cache = Arc::new(PrioritizationFeeCache::new(0u64));
        let pool = SchedulerPool::<
            AsyncScheduler<TRIGGER_RACE_CONDITION>,
            DefaultTaskHandler,
            DefaultScheduleExecutionArg,
        >::new_dyn(None, None, None, ignored_prioritization_fee_cache);
        let scheduler = pool.take_scheduler(context);

        let bank = BankWithScheduler::new(bank, Some(scheduler));
        assert_eq!(bank.transaction_count(), 0);

        // schedule but not immediately execute transaction
        bank.schedule_transaction_executions([(&very_old_valid_tx, &0)].into_iter());
        // this calls register_recent_blockhash internally
        bank.fill_bank_with_ticks_for_tests();

        if TRIGGER_RACE_CONDITION {
            // very_old_valid_tx is wrongly handled as expired!
            assert_matches!(
                bank.wait_for_completed_scheduler(),
                Some((Err(TransactionError::BlockhashNotFound), _))
            );
            assert_eq!(bank.transaction_count(), 0);
        } else {
            assert_matches!(bank.wait_for_completed_scheduler(), Some((Ok(()), _)));
            assert_eq!(bank.transaction_count(), 1);
        }
    }

    #[test]
    fn test_scheduler_schedule_execution_recent_blockhash_edge_case_with_race() {
        do_test_scheduler_schedule_execution_recent_blockhash_edge_case::<true>();
    }

    #[test]
    fn test_scheduler_schedule_execution_recent_blockhash_edge_case_without_race() {
        do_test_scheduler_schedule_execution_recent_blockhash_edge_case::<false>();
    }
}
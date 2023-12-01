//! Transaction scheduling code.
//!
//! This crate implements two solana-runtime traits (`InstalledScheduler` and
//! `InstalledSchedulerPool`) to provide concrete transaction scheduling implementation (including
//! executing txes and committing tx results).
//!
//! At highest level, this crate takes `SanitizedTransaction`s via its `schedule_execution()` and
//! commits any side-effects (i.e. on-chain state changes) into `Bank`s via `solana-ledger`'s
//! helper fun called `execute_batch()`.

use {
    rand::{thread_rng, Rng},
    solana_ledger::blockstore_processor::{
        execute_batch, TransactionBatchWithIndexes, TransactionStatusSender,
    },
    solana_program_runtime::timings::ExecuteTimings,
    solana_runtime::{
        bank::Bank,
        installed_scheduler_pool::{
            DefaultScheduleExecutionArg, InstalledScheduler, InstalledSchedulerPool,
            InstalledSchedulerPoolArc, ResultWithTimings, ScheduleExecutionArg, SchedulerId,
            SchedulingContext, SchedulingMode, WaitReason, WithSchedulingMode,
            WithTransactionAndIndex,
        },
        prioritization_fee_cache::PrioritizationFeeCache,
    },
    solana_sdk::transaction::{Result, SanitizedTransaction},
    solana_vote::vote_sender_types::ReplayVoteSender,
    std::{
        fmt::Debug,
        marker::PhantomData,
        sync::{Arc, Mutex, Weak},
    },
};

// SchedulerPool must be accessed via dyn by solana-runtime code, because of its internal fields'
// types (currently TransactionStatusSender; also, PohRecorder in the future) aren't available
// there...
#[derive(Debug)]
pub struct SchedulerPool<
    T: SpawnableScheduler<TH, SEA>,
    TH: ScheduledTransactionHandler<SEA>,
    SEA: ScheduleExecutionArg,
> {
    schedulers: Mutex<Vec<Box<T>>>,
    log_messages_bytes_limit: Option<usize>,
    transaction_status_sender: Option<TransactionStatusSender>,
    replay_vote_sender: Option<ReplayVoteSender>,
    prioritization_fee_cache: Arc<PrioritizationFeeCache>,
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
    _phantom: PhantomData<(T, TH, SEA)>,
}

pub type DefaultSchedulerPool = SchedulerPool<
    PooledScheduler<DefaultTransactionHandler, DefaultScheduleExecutionArg>,
    DefaultTransactionHandler,
    DefaultScheduleExecutionArg,
>;

impl<
        T: SpawnableScheduler<TH, SEA>,
        TH: ScheduledTransactionHandler<SEA>,
        SEA: ScheduleExecutionArg,
    > SchedulerPool<T, TH, SEA>
{
    pub fn new(
        log_messages_bytes_limit: Option<usize>,
        transaction_status_sender: Option<TransactionStatusSender>,
        replay_vote_sender: Option<ReplayVoteSender>,
        prioritization_fee_cache: Arc<PrioritizationFeeCache>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|weak_self| Self {
            schedulers: Mutex::default(),
            log_messages_bytes_limit,
            transaction_status_sender,
            replay_vote_sender,
            prioritization_fee_cache,
            weak_self: weak_self.clone(),
            _phantom: PhantomData,
        })
    }

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

    // See a comment at the weak_self field for justification of this.
    pub fn self_arc(&self) -> Arc<Self> {
        self.weak_self
            .upgrade()
            .expect("self-referencing Arc-ed pool")
    }

    pub fn return_scheduler(&self, scheduler: Box<T>) {
        assert!(!scheduler.has_context());

        self.schedulers
            .lock()
            .expect("not poisoned")
            .push(scheduler);
    }

    pub fn do_take_scheduler(&self, context: SchedulingContext) -> Box<T> {
        // pop is intentional for filo, expecting relatively warmed-up scheduler due to having been
        // returned recently
        if let Some(mut scheduler) = self.schedulers.lock().expect("not poisoned").pop() {
            scheduler.replace_context(context);
            scheduler
        } else {
            Box::new(T::spawn(self.self_arc(), context, TH::create(self)))
        }
    }
}

impl<
        T: SpawnableScheduler<TH, SEA>,
        TH: ScheduledTransactionHandler<SEA>,
        SEA: ScheduleExecutionArg,
    > InstalledSchedulerPool<SEA> for SchedulerPool<T, TH, SEA>
{
    fn take_scheduler(&self, context: SchedulingContext) -> Box<dyn InstalledScheduler<SEA>> {
        self.do_take_scheduler(context)
    }
}

pub trait ScheduledTransactionHandler<SEA: ScheduleExecutionArg>:
    Send + Sync + Debug + Sized + 'static
{
    fn create<T: SpawnableScheduler<Self, SEA>>(pool: &SchedulerPool<T, Self, SEA>) -> Self;

    fn handle<T: SpawnableScheduler<Self, SEA>>(
        &self,
        result: &mut Result<()>,
        timings: &mut ExecuteTimings,
        bank: &Arc<Bank>,
        transaction: &SanitizedTransaction,
        index: usize,
        pool: &SchedulerPool<T, Self, SEA>,
    );
}

#[derive(Debug)]
pub struct DefaultTransactionHandler;

impl<SEA: ScheduleExecutionArg> ScheduledTransactionHandler<SEA> for DefaultTransactionHandler {
    fn create<T: SpawnableScheduler<Self, SEA>>(_pool: &SchedulerPool<T, Self, SEA>) -> Self {
        Self
    }

    fn handle<T: SpawnableScheduler<Self, SEA>>(
        &self,
        result: &mut Result<()>,
        timings: &mut ExecuteTimings,
        bank: &Arc<Bank>,
        transaction: &SanitizedTransaction,
        index: usize,
        pool: &SchedulerPool<T, Self, SEA>,
    ) {
        // scheduler must properly prevent conflicting tx executions, so locking isn't needed
        // here
        let batch = bank.prepare_unlocked_batch_from_single_tx(transaction);
        let batch_with_indexes = TransactionBatchWithIndexes {
            batch,
            transaction_indexes: vec![index],
        };

        *result = execute_batch(
            &batch_with_indexes,
            bank,
            pool.transaction_status_sender.as_ref(),
            pool.replay_vote_sender.as_ref(),
            timings,
            pool.log_messages_bytes_limit,
            &pool.prioritization_fee_cache,
        );
    }
}

// Currently, simplest possible implementation (i.e. single-threaded)
// this will be replaced with more proper implementation...
// not usable at all, especially for mainnet-beta
#[derive(Debug)]
pub struct PooledScheduler<TH: ScheduledTransactionHandler<SEA>, SEA: ScheduleExecutionArg> {
    id: SchedulerId,
    pool: Arc<SchedulerPool<Self, TH, SEA>>,
    context: Option<SchedulingContext>,
    result_with_timings: Mutex<Option<ResultWithTimings>>,
    handler: TH,
    _phantom: PhantomData<SEA>,
}

impl<TH: ScheduledTransactionHandler<SEA>, SEA: ScheduleExecutionArg> PooledScheduler<TH, SEA> {
    pub fn do_spawn(
        pool: Arc<SchedulerPool<Self, TH, SEA>>,
        initial_context: SchedulingContext,
        handler: TH,
    ) -> Self {
        Self {
            id: thread_rng().gen::<SchedulerId>(),
            pool,
            context: Some(initial_context),
            result_with_timings: Mutex::default(),
            handler,
            _phantom: PhantomData,
        }
    }
}

pub trait InstallableScheduler<SEA: ScheduleExecutionArg>: InstalledScheduler<SEA> {
    fn has_context(&self) -> bool;
    fn replace_context(&mut self, context: SchedulingContext);
}

pub trait SpawnableScheduler<TH: ScheduledTransactionHandler<SEA>, SEA: ScheduleExecutionArg>:
    InstallableScheduler<SEA>
{
    fn spawn(
        pool: Arc<SchedulerPool<Self, TH, SEA>>,
        initial_context: SchedulingContext,
        handler: TH,
    ) -> Self
    where
        Self: Sized;
}

impl<TH: ScheduledTransactionHandler<SEA>, SEA: ScheduleExecutionArg> SpawnableScheduler<TH, SEA>
    for PooledScheduler<TH, SEA>
{
    fn spawn(
        pool: Arc<SchedulerPool<Self, TH, SEA>>,
        initial_context: SchedulingContext,
        handler: TH,
    ) -> Self {
        Self::do_spawn(pool, initial_context, handler)
    }
}

impl<TH: ScheduledTransactionHandler<SEA>, SEA: ScheduleExecutionArg> InstalledScheduler<SEA>
    for PooledScheduler<TH, SEA>
{
    fn id(&self) -> SchedulerId {
        self.id
    }

    fn context(&self) -> &SchedulingContext {
        self.context.as_ref().expect("active context should exist")
    }

    fn schedule_execution(&self, transaction_with_index: SEA::TransactionWithIndex<'_>) {
        let fail_fast = match self.context().mode() {
            // this should be false, for (upcoming) BlockProduction variant.
            SchedulingMode::BlockVerification => true,
        };

        let result_with_timings = &mut *self.result_with_timings.lock().expect("not poisoned");
        let (result, timings) =
            result_with_timings.get_or_insert_with(|| (Ok(()), ExecuteTimings::default()));

        // so, we're NOT scheduling at all; rather, just execute tx straight off.  we doesn't need
        // to solve inter-tx locking deps only in the case of single-thread fifo like this....
        if result.is_ok() || !fail_fast {
            transaction_with_index.with_transaction_and_index(|transaction, index| {
                TH::handle(
                    &self.handler,
                    result,
                    timings,
                    self.context().bank(),
                    transaction,
                    index,
                    &self.pool,
                );
            })
        }
    }

    fn wait_for_termination(&mut self, wait_reason: &WaitReason) -> Option<ResultWithTimings> {
        let keep_result_with_timings = wait_reason.is_paused();

        if keep_result_with_timings {
            None
        } else {
            drop(
                self.context
                    .take()
                    .expect("active context should be dropped"),
            );
            // current simplest form of this trait impl doesn't block the current thread materially
            // just with the following single mutex lock. Suppose more elaborated synchronization
            // across worker threads here in the future...
            self.result_with_timings
                .lock()
                .expect("not poisoned")
                .take()
        }
    }

    fn return_to_pool(self: Box<Self>) {
        self.pool.clone().return_scheduler(self)
    }
}

impl<TH: ScheduledTransactionHandler<SEA>, SEA: ScheduleExecutionArg> InstallableScheduler<SEA>
    for PooledScheduler<TH, SEA>
{
    fn has_context(&self) -> bool {
        self.context.is_some()
    }

    fn replace_context(&mut self, context: SchedulingContext) {
        self.context = Some(context);
        *self.result_with_timings.lock().expect("not poisoned") = None;
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assert_matches::assert_matches,
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

        let mut scheduler1 = pool.do_take_scheduler(context.clone());
        let scheduler_id1 = scheduler1.id();
        let mut scheduler2 = pool.do_take_scheduler(context.clone());
        let scheduler_id2 = scheduler2.id();
        assert_ne!(scheduler_id1, scheduler_id2);

        assert_matches!(
            scheduler1.wait_for_termination(&WaitReason::TerminatedToFreeze),
            None
        );
        pool.return_scheduler(scheduler1);
        assert_matches!(
            scheduler2.wait_for_termination(&WaitReason::TerminatedToFreeze),
            None
        );
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

        assert!(scheduler.has_context());
        assert_matches!(
            scheduler.wait_for_termination(&WaitReason::PausedForRecentBlockhash),
            None
        );
        assert!(scheduler.has_context());
        assert_matches!(
            scheduler.wait_for_termination(&WaitReason::TerminatedToFreeze),
            None
        );
        assert!(!scheduler.has_context());
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

        let mut scheduler = pool.do_take_scheduler(old_context.clone());
        let scheduler_id = scheduler.id();
        assert_matches!(
            scheduler.wait_for_termination(&WaitReason::TerminatedToFreeze),
            None
        );
        pool.return_scheduler(scheduler);

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
        let bank = Arc::new(Bank::new_for_tests(&genesis_config));
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
        let unfunded_keypair = Keypair::new();
        let tx0 = &SanitizedTransaction::from_transaction_for_tests(system_transaction::transfer(
            &unfunded_keypair,
            &solana_sdk::pubkey::new_rand(),
            2,
            genesis_config.hash(),
        ));
        let bank = Arc::new(Bank::new_for_tests(&genesis_config));
        let ignored_prioritization_fee_cache = Arc::new(PrioritizationFeeCache::new(0u64));
        let pool =
            DefaultSchedulerPool::new_dyn(None, None, None, ignored_prioritization_fee_cache);
        let context = SchedulingContext::new(SchedulingMode::BlockVerification, bank.clone());

        assert_eq!(bank.transaction_count(), 0);
        let scheduler = pool.take_scheduler(context);
        scheduler.schedule_execution(&(tx0, 0));
        assert_eq!(bank.transaction_count(), 0);

        let tx1 = &SanitizedTransaction::from_transaction_for_tests(system_transaction::transfer(
            &mint_keypair,
            &solana_sdk::pubkey::new_rand(),
            3,
            genesis_config.hash(),
        ));
        assert_matches!(
            bank.simulate_transaction_unchecked(tx1.clone()).result,
            Ok(_)
        );
        scheduler.schedule_execution(&(tx1, 0));
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
        PooledScheduler<DefaultTransactionHandler, DefaultScheduleExecutionArg>,
        Mutex<Vec<JoinHandle<ResultWithTimings>>>,
    );

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
            &(transaction, index): <DefaultScheduleExecutionArg as ScheduleExecutionArg>::TransactionWithIndex<'a>,
        ) {
            let transaction_and_index = (transaction.clone(), index);
            let context = self.context().clone();
            let pool = self.0.pool.clone();

            self.1.lock().unwrap().push(std::thread::spawn(move || {
                // intentionally sleep to simulate race condition where register_recent_blockhash
                // is run before finishing executing scheduled transactions
                std::thread::sleep(std::time::Duration::from_secs(1));

                let mut result = Ok(());
                let mut timings = ExecuteTimings::default();

                <DefaultTransactionHandler as ScheduledTransactionHandler<
                    DefaultScheduleExecutionArg,
                >>::handle(
                    &DefaultTransactionHandler,
                    &mut result,
                    &mut timings,
                    context.bank(),
                    &transaction_and_index.0,
                    transaction_and_index.1,
                    &pool,
                );
                (result, timings)
            }));
        }

        fn wait_for_termination(&mut self, reason: &WaitReason) -> Option<ResultWithTimings> {
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
        }

        fn return_to_pool(self: Box<Self>) {
            Box::new(self.0).return_to_pool()
        }
    }

    impl<const TRIGGER_RACE_CONDITION: bool>
        SpawnableScheduler<DefaultTransactionHandler, DefaultScheduleExecutionArg>
        for AsyncScheduler<TRIGGER_RACE_CONDITION>
    {
        fn spawn(
            pool: Arc<SchedulerPool<Self, DefaultTransactionHandler, DefaultScheduleExecutionArg>>,
            initial_context: SchedulingContext,
            handler: DefaultTransactionHandler,
        ) -> Self {
            AsyncScheduler::<TRIGGER_RACE_CONDITION>(
                PooledScheduler::<DefaultTransactionHandler, DefaultScheduleExecutionArg> {
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
        }
    }

    impl<const TRIGGER_RACE_CONDITION: bool> InstallableScheduler<DefaultScheduleExecutionArg>
        for AsyncScheduler<TRIGGER_RACE_CONDITION>
    {
        fn has_context(&self) -> bool {
            self.0.has_context()
        }

        fn replace_context(&mut self, context: SchedulingContext) {
            self.0.replace_context(context)
        }
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
        let mut bank = Arc::new(Bank::new_for_tests(&genesis_config));
        for _ in 0..MAX_PROCESSING_AGE {
            bank.fill_bank_with_ticks_for_tests();
            bank.freeze();
            bank = Arc::new(Bank::new_from_parent(
                bank.clone(),
                &Pubkey::default(),
                bank.slot().checked_add(1).unwrap(),
            ));
        }
        let context = SchedulingContext::new(SchedulingMode::BlockVerification, bank.clone());

        let ignored_prioritization_fee_cache = Arc::new(PrioritizationFeeCache::new(0u64));
        let pool = SchedulerPool::<
            AsyncScheduler<TRIGGER_RACE_CONDITION>,
            DefaultTransactionHandler,
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
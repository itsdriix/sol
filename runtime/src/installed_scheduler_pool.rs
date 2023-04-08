//! Transaction processing glue code, mainly consisting of Object-safe traits
//!
//! `trait InstalledSchedulerPool` is the most crucial piece of code for this whole integration.
//!
//! It lends one of pooled `trait InstalledScheduler`s out to a `Bank`, so that the ubiquitous
//! `Arc<Bank>` can conveniently work as a facade for transaction scheduling, to higher-layer
//! subsystems (i.e. both `ReplayStage` and `BankingStage`). `BankForks` is responsible for this
//! book-keeping, including returning the scheduler from the bank to the pool after use.
//!
//! `trait InstalledScheduler` can be fed with `SanitizedTransaction`s. Then, it schedules and
//! commits those transaction execution results into the associated _bank_. That means,
//! `InstalledScheduler` and `Bank` are mutually linked to each other, resulting in somewhat
//! special handling as part of their life-cycle.
//!
//! Albeit being at this abstract interface level, it's generally assumed that each
//! `InstalledScheduler` is backed by multiple threads for performant transaction execution and
//! there're multiple independent schedulers inside a single instance of `InstalledSchedulerPool`.
//!
//! Dynamic dispatch was inevitable, due to the need of delegating those implementations to the
//! dependent crate (`solana-scheduler-pool`, which in turn depends on `solana-ledger`; another
//! dependent crate of `solana-runtime`...), while cutting cyclic dependency.

use {
    crate::{bank::Bank, bank_forks::BankForks},
    log::*,
    solana_program_runtime::timings::ExecuteTimings,
    solana_scheduler::{SchedulingMode, WithSchedulingMode},
    solana_sdk::{
        slot_history::Slot,
        transaction::{Result, SanitizedTransaction},
    },
    std::{fmt::Debug, ops::Deref, sync::Arc},
};

pub trait InstalledSchedulerPool: Send + Sync + Debug {
    fn take_from_pool(&self, context: SchedulingContext) -> SchedulerBox;
    fn return_to_pool(&self, scheduler: SchedulerBox);
}

pub(crate) type SchedulerPoolBox = Box<dyn InstalledSchedulerPool>;
pub(crate) type InstalledSchedulerPoolBox = Option<SchedulerPoolBox>;

pub type SchedulerId = u64;

pub trait InstalledScheduler: Send + Sync + Debug {
    fn scheduler_id(&self) -> SchedulerId;
    fn scheduler_pool(&self) -> SchedulerPoolBox;

    fn schedule_execution(&self, sanitized_tx: &SanitizedTransaction, index: usize);
    fn schedule_termination(&mut self);
    fn wait_for_termination(
        &mut self,
        from_internal: bool,
        is_restart: bool,
    ) -> Option<(ExecuteTimings, Result<()>)>;

    fn replace_scheduler_context(&self, context: SchedulingContext);
}

pub type SchedulerBox = Box<dyn InstalledScheduler>;
// somewhat arbitrarily new type just to pacify Bank's frozen_abi...
#[derive(Debug, Default)]
pub(crate) struct InstalledSchedulerBox(pub(crate) Option<SchedulerBox>);

#[cfg(RUSTC_WITH_SPECIALIZATION)]
use solana_frozen_abi::abi_example::AbiExample;

#[cfg(RUSTC_WITH_SPECIALIZATION)]
impl AbiExample for InstalledSchedulerBox {
    fn example() -> Self {
        Self(None)
    }
}

#[derive(Clone, Debug)]
pub struct SchedulingContext {
    mode: SchedulingMode,
    bank: Arc<Bank>,
}

impl WithSchedulingMode for SchedulingContext {
    fn mode(&self) -> SchedulingMode {
        self.mode
    }
}

impl SchedulingContext {
    pub fn new(mode: SchedulingMode, bank: Arc<Bank>) -> Self {
        Self { mode, bank }
    }

    pub fn bank(&self) -> &Arc<Bank> {
        &self.bank
    }

    pub fn slot(&self) -> Slot {
        self.bank().slot()
    }

    pub fn log_prefix(scheduler_id: u64, context: Option<&Self>) -> String {
        const BITS_PER_HEX_DIGIT: u32 = 4;

        format!(
            "id_{:width$x}{}",
            scheduler_id,
            context
                .as_ref()
                .map(|c| format!(" slot: {}, mode: {:?}", c.slot(), c.mode))
                .unwrap_or_else(|| "".into()),
            width = SchedulerId::BITS / BITS_PER_HEX_DIGIT,
        )
    }

    pub fn into_bank(self) -> Option<Bank> {
        // XXX: this is racy....
        Arc::try_unwrap(self.bank).ok()
    }
}

pub(crate) struct BankWithScheduler(pub(crate) Arc<Bank>);

impl BankWithScheduler {
    pub(crate) fn new_arc(&self) -> Arc<Bank> {
        self.0.clone()
    }

    pub(crate) fn into_arc(self) -> Arc<Bank> {
        let bank = self.new_arc();
        drop(self);
        bank
    }
}

impl Drop for BankWithScheduler {
    fn drop(&mut self) {
        self.0.schedule_termination();
    }
}

impl Deref for BankWithScheduler {
    type Target = Bank;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl BankForks {
    pub fn install_scheduler_pool(&mut self, pool: SchedulerPoolBox) {
        info!("Installed new scheduler_pool into bank_forks: {:?}", pool);
        assert!(self.scheduler_pool.replace(pool).is_none());
    }

    pub(crate) fn install_scheduler_into_bank(&self, bank: &Arc<Bank>) {
        if let Some(scheduler_pool) = &self.scheduler_pool {
            let new_context =
                SchedulingContext::new(SchedulingMode::BlockVerification, bank.clone());
            bank.install_scheduler(scheduler_pool.take_from_pool(new_context));
        }
    }
}

impl Bank {
    pub(crate) fn install_scheduler(&self, scheduler: SchedulerBox) {
        let mut scheduler_guard = self.scheduler.write().unwrap();
        assert!(scheduler_guard.0.replace(scheduler).is_none());
    }

    pub fn with_scheduler(&self) -> bool {
        self.scheduler.read().unwrap().0.is_some()
    }

    pub fn schedule_transaction_executions<'a>(
        &self,
        transactions: &[SanitizedTransaction],
        transaction_indexes: impl Iterator<Item = &'a usize>,
    ) {
        trace!(
            "schedule_transaction_executions(): {} txs",
            transactions.len()
        );

        let scheduler_guard = self.scheduler.read().unwrap();
        let scheduler = scheduler_guard.0.as_ref().unwrap();

        for (sanitized_transaction, &index) in transactions.iter().zip(transaction_indexes) {
            scheduler.schedule_execution(sanitized_transaction, index);
        }
    }

    pub(crate) fn schedule_termination(&self) {
        let mut scheduler_guard = self.scheduler.write().unwrap();
        if let Some(scheduler) = scheduler_guard.0.as_mut() {
            scheduler.schedule_termination();
        }
    }

    fn wait_for_scheduler<
        const VIA_DROP: bool,
        const FROM_INTERNAL: bool,
        const IS_RESTART: bool,
    >(
        &self,
    ) -> Option<(ExecuteTimings, Result<()>)> {
        let mut scheduler_guard = self.scheduler.write().unwrap();
        if scheduler_guard.0.is_some() {
            let timings_and_result = scheduler_guard
                .0
                .as_mut()
                .and_then(|scheduler| scheduler.wait_for_termination(FROM_INTERNAL, IS_RESTART));
            if !IS_RESTART {
                if let Some(scheduler) = scheduler_guard.0.take() {
                    scheduler.scheduler_pool().return_to_pool(scheduler);
                }
            }
            timings_and_result
        } else {
            None
        }
    }

    pub fn wait_for_completed_scheduler(&self) -> (ExecuteTimings, Result<()>) {
        let maybe_timings_and_result = self.wait_for_scheduler::<false, false, false>();
        maybe_timings_and_result.unwrap_or((ExecuteTimings::default(), Ok(())))
    }

    fn wait_for_completed_scheduler_via_drop(&self) -> Option<Result<()>> {
        let maybe_timings_and_result = self.wait_for_scheduler::<true, false, false>();
        maybe_timings_and_result.map(|(_timings, result)| result)
    }

    pub fn wait_for_completed_scheduler_via_internal_drop(self) {
        let maybe_timings_and_result = self.wait_for_scheduler::<true, true, false>();
        assert!(maybe_timings_and_result.is_some());
    }

    pub(crate) fn wait_for_reusable_scheduler(&self) {
        let maybe_timings_and_result = self.wait_for_scheduler::<false, false, true>();
        assert!(maybe_timings_and_result.is_none());
    }

    pub(crate) fn drop_scheduler(&mut self) {
        if self.with_scheduler() {
            if let Some(Err(err)) = self.wait_for_completed_scheduler_via_drop() {
                warn!(
                    "Bank::drop(): slot: {} discarding error from scheduler: {:?}",
                    self.slot(),
                    err
                );
            }
        }
    }
}

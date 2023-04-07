use std::sync::Arc;
use crate::bank::Bank;
use solana_sdk::slot_history::Slot;
use solana_sdk::transaction::Result;
use solana_sdk::transaction::SanitizedTransaction;
use solana_program_runtime::timings::ExecuteTimings;


pub trait InstalledSchedulerPool: Send + Sync + std::fmt::Debug {
    fn take_from_pool(&self, context: SchedulingContext) -> Box<dyn InstalledScheduler>;
    fn return_to_pool(&self, scheduler: Box<dyn InstalledScheduler>);
    // drop with exit atomicbool integration??
}

pub trait InstalledScheduler: Send + Sync + std::fmt::Debug {
    fn random_id(&self) -> u64;
    fn scheduler_pool(&self) -> Box<dyn InstalledSchedulerPool>;

    fn schedule_execution(&self, sanitized_tx: &SanitizedTransaction, index: usize);
    fn schedule_termination(&mut self);
    fn wait_for_termination(
        &mut self,
        from_internal: bool,
        is_restart: bool,
    ) -> Option<(ExecuteTimings, Result<()>)>;

    fn replace_scheduler_context(&self, context: SchedulingContext);

    // drop with exit atomicbool integration??
}

#[derive(Debug, Default)]
pub(crate) struct InstalledSchedulerBox(pub(crate) Option<Box<dyn InstalledScheduler>>);

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
    pub bank: std::sync::Arc<Bank>,
    pub mode: solana_scheduler::Mode,
}

impl solana_scheduler::WithMode for SchedulingContext {
    fn mode(&self) -> solana_scheduler::Mode {
        self.mode
    }
}

impl SchedulingContext {
    pub fn new(bank: Arc<Bank>, mode: solana_scheduler::Mode) -> Self {
        Self { bank, mode }
    }

    pub fn slot(&self) -> Slot {
        self.bank().slot()
    }

    pub fn bank(&self) -> &Arc<Bank> {
        &self.bank
    }

    pub fn log_prefix(random_id: u64, context: Option<&Self>) -> String {
        format!(
            "id_{:016x}{}",
            random_id,
            context
                .as_ref()
                .map(|c| format!(" slot: {}, mode: {:?}", c.slot(), c.mode))
                .unwrap_or_else(|| "".into())
        )
    }

    pub fn into_bank(self) -> Option<Bank> {
        Arc::try_unwrap(self.bank).ok()
    }
}


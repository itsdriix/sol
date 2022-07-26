#![feature(map_first_last)]

use {
    atomic_enum::atomic_enum,
    crossbeam_channel::{bounded, unbounded},
    log::*,
    rand::Rng,
    sha2::{Digest, Sha256},
    solana_entry::entry::Entry,
    solana_measure::measure::Measure,
    solana_metrics::datapoint_info,
    solana_sdk::{pubkey::Pubkey, transaction::SanitizedTransaction},
};
use solana_sdk::transaction::VersionedTransaction;
use solana_sdk::transaction::TransactionAccountLocks;
use solana_sdk::hash::Hash;

#[derive(Default, Debug)]
struct ExecutionEnvironment {
    lock_attempts: Vec<LockAttempt>,
    accounts: Vec<i8>,
    cu: usize,
    //tx: Tx,
}

impl ExecutionEnvironment {
    fn new(cu: usize) -> Self {
        Self {
            cu,
            ..Self::default()
        }
    }

    //fn abort() {
    //  pass AtomicBool into InvokeContext??
    //}
}

#[derive(Debug)]
struct LockAttempt {
    address: Pubkey,
    is_success: bool,
    requested_usage: RequestedUsage,
}

impl LockAttempt {
    fn is_success(&self) -> bool {
        self.is_success
    }

    fn is_failure(&self) -> bool {
        !self.is_success()
    }

    fn success(address: Pubkey, requested_usage: RequestedUsage) -> Self {
        Self { address, is_success: true, requested_usage }
    }

    fn failure(address: Pubkey, requested_usage: RequestedUsage) -> Self {
        Self { address, is_success: false, requested_usage }
    }
}

type UsageCount = usize;
const SOLE_USE_COUNT: UsageCount = 1;

#[derive(PartialEq)]
enum CurrentUsage {
    Unused,
    // weight to abort running tx?
    // also sum all readonly weights to subvert to write lock with greater weight?
    Readonly(UsageCount),
    Writable,
}

impl CurrentUsage {
    fn renew(requested_usage: RequestedUsage) -> Self {
        match requested_usage {
            RequestedUsage::Readonly => {
                CurrentUsage::Readonly(SOLE_USE_COUNT)
            }
            RequestedUsage::Writable => {
                CurrentUsage::Writable
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RequestedUsage {
    Readonly,
    Writable,
}

struct Page {
    current_usage: CurrentUsage,
    contended_queue: std::collections::BTreeSet<UniqueWeight>,
    //next_scheduled_task
}

type AddressBookMap = std::collections::BTreeMap<Pubkey, Page>;

// needs ttl mechanism and prune
struct AddressBook {
    map: AddressBookMap,
    newly_uncontended_addresses: std::collections::BTreeSet<Pubkey>,
}

impl AddressBook {
    fn attempt_lock_address(&mut self, unique_weight: &UniqueWeight, address: Pubkey, requested_usage: RequestedUsage) -> LockAttempt {
        use std::collections::btree_map::Entry;

        match self.map.entry(address) {
            // unconditional success if it's initial access
            Entry::Vacant(entry) => {
                entry.insert(Page {
                    current_usage: CurrentUsage::renew(requested_usage),
                    contended_queue: Default::default(),
                });
                LockAttempt::success(address, requested_usage)
            }
            Entry::Occupied(mut entry) => {
                let mut page = entry.get_mut();

                match &mut page.current_usage {
                    CurrentUsage::Unused => {
                        page.current_usage = CurrentUsage::renew(requested_usage);
                        LockAttempt::success(address, requested_usage) 
                    }
                    CurrentUsage::Readonly(ref mut current_count) => {
                        match &requested_usage {
                            RequestedUsage::Readonly => {
                                *current_count += 1;
                                LockAttempt::success(address, requested_usage)
                            },
                            RequestedUsage::Writable => {
                                // add to contended queue?
                                LockAttempt::failure(address, requested_usage)
                            }
                        }
                    }
                    CurrentUsage::Writable => {
                        match &requested_usage {
                            RequestedUsage::Readonly | RequestedUsage::Writable => {
                                page.contended_queue.insert((*unique_weight).clone());
                                LockAttempt::failure(address, requested_usage)
                            }
                        }
                    }
                }
            }
        }
    }

    fn ensure_unlock(&mut self, attempt: &LockAttempt) {
        if attempt.is_success() {
            self.unlock(attempt);
        }
    }

    fn unlock(&mut self, attempt: &LockAttempt) -> bool {
        assert!(attempt.is_success());

        use std::collections::btree_map::Entry;
        let mut newly_uncontended = false;

        match self.map.entry(attempt.address) {
            Entry::Occupied(mut entry) => {
                let mut page = entry.get_mut();

                match &mut page.current_usage {
                    CurrentUsage::Readonly(ref mut current_count) => {
                        match &attempt.requested_usage {
                            RequestedUsage::Readonly => {
                                if *current_count == SOLE_USE_COUNT {
                                    newly_uncontended = true;
                                } else {
                                    *current_count -= 1;
                                }
                            },
                            RequestedUsage::Writable => unreachable!() 
                        }
                    }
                    CurrentUsage::Writable => {
                        match &attempt.requested_usage {
                            RequestedUsage::Writable => {
                                newly_uncontended = true;
                            }
                            RequestedUsage::Readonly => unreachable!(),
                        }
                    }
                    CurrentUsage::Unused => unreachable!(),
                }

                if newly_uncontended {
                    page.current_usage = CurrentUsage::Unused;
                }
            }
            Entry::Vacant(entry) => {
                unreachable!()
            }
        }

        newly_uncontended
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Weight { // naming: Sequence Ordering?
    ix: usize, // index in ledger entry?
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct UniqueWeight { // naming: Sequence Ordering?
    weight: Weight,
    // we can't use Transaction::message_hash because it's manipulatable to be favorous to the tx
    // submitter
    unique_key: Hash, // tie breaker? random noise? also for unique identification of txes?
    // fee?
}

struct Bundle {
    // what about bundle1{tx1a, tx2} and bundle2{tx1b, tx2}?
}

struct Task {
    tx: SanitizedTransaction, // actually should be Bundle
}

// RunnableQueue, ContendedQueue?
struct TransactionQueue {
    map: std::collections::BTreeMap<UniqueWeight, Task>,
}

struct ContendedQueue {
    map: std::collections::BTreeMap<UniqueWeight, Task>,
}

impl TransactionQueue {
    fn add(&mut self, unique_weight: UniqueWeight, task: Task) {
        self.map.insert(unique_weight, task).unwrap();
    }

    fn pop_next_task(&mut self) -> Option<(UniqueWeight, Task)> {
        self.map.pop_last()
    }
}

fn attempt_lock_for_execution<'a>(
    address_book: &mut AddressBook,
    unique_weight: &UniqueWeight,
    message_hash: &'a Hash,
    locks: &'a TransactionAccountLocks,
) -> Vec<LockAttempt> {
    // no short-cuircuit; we at least all need to add to the contended queue
    let mut writable_attempts = locks
        .writable
        .iter()
        .cloned()
        .map(|&a| address_book.attempt_lock_address(unique_weight, a, RequestedUsage::Writable))
        .collect::<Vec<_>>();

    let mut readonly_attempts = locks
        .readonly
        .iter()
        .cloned()
        .map(|&a| address_book.attempt_lock_address(unique_weight, a, RequestedUsage::Readonly))
        .collect::<Vec<_>>();

    writable_attempts.append(&mut readonly_attempts);
    writable_attempts
}

fn ensure_unlock_for_failed_execution(address_book: &mut AddressBook, lock_attempts: Vec<LockAttempt>) {
    for l in lock_attempts {
        address_book.ensure_unlock(&l)
        // mem::forget and panic in LockAttempt::drop()
    }
}

fn unlock_after_execution(address_book: &mut AddressBook, lock_attempts: Vec<LockAttempt>) {
    for l in lock_attempts {
        let newly_uncontended = address_book.unlock(&l);
        if newly_uncontended {
            address_book.newly_uncontended_addresses.insert(l.address);
        }
        // mem::forget and panic in LockAttempt::drop()
    }
}

fn send_to_execution_stage(ee: ExecutionEnvironment) {}

fn main() {
    solana_logger::setup();
    error!("hello");
    let thread_count = 10;
    let (s, r) = bounded::<(
        (usize, usize, (std::time::Instant, ExecutionEnvironment)),
        Vec<u8>,
    )>(thread_count * 10);
    let (s2, r2) = bounded(thread_count * 2);

    /*
    let p = std::thread::Builder::new().name("producer".to_string()).spawn(move || {
        let mut rng = rand::thread_rng();
        loop {
            s2.send((std::time::Instant::now(), ExecutionEnvironment::new(rng.gen_range(0, 1000)))).unwrap();
        }
    }).unwrap();
    */
    let pc = std::thread::Builder::new().name("prosumer".to_string()).spawn(move || {
        use crossbeam_channel::select;

        let mut rng = rand::thread_rng();
        let mut count = 0;
        let start = std::time::Instant::now();
        //let mut rrr = Vec::with_capacity(10);
        //for _ in 0..100 {
        let mut elapsed = 0;

        loop {
            select! {
                send(s2, (std::time::Instant::now(), ExecutionEnvironment::new(rng.gen_range(0, 1000)))) -> res => {
                    res.unwrap();
                }
                recv(r) -> msg => {
                    let rr = msg.unwrap();
                    elapsed += rr.0.2.0.elapsed().as_nanos();
                    //    rrr.push((rr.0.2.0.elapsed(), rr));
                    //}

                    //for rr in rrr {
                    count += 1;
                    //error!("recv-ed: {:?}", &rr);
                    if count % 100_000 == 0 {
                        error!("recv-ed: {}", count / start.elapsed().as_secs().max(1));
                        //break
                    }
                }
                }
        }
    }).unwrap();

    let mut joins = (0..thread_count)
        .map(|thx| {
            let s = s.clone();
            let r2 = r2.clone();
            std::thread::Builder::new()
                .name(format!("blockstore_processor_{}", thx))
                .spawn(move || {
                    let current_thread_name = std::thread::current().name().unwrap().to_string();
                    let mut i = 0;
                    //for _ in 0..60 {//000000 {
                    loop {
                        let ss = (thx, i, r2.recv().unwrap());

                        let mut process_message_time = Measure::start("process_message_time");

                        let mut hasher = Sha256::default();
                        let cu = ss.2 .1.cu;
                        for i in 0_usize..cu {
                            //for _ in 0..10 {
                            hasher.update(i.to_le_bytes());
                            //}
                        }
                        let h = hasher.finalize();

                        process_message_time.stop();
                        let duration_with_overhead = process_message_time.as_us();

                        /*
                        datapoint_info!(
                            "individual_tx_stats",
                            ("slot", 33333, i64),
                            ("thread", current_thread_name, String),
                            ("signature", "ffffff", String),
                            ("account_locks_in_json", "{}", String),
                            ("status", "Ok", String),
                            ("duration", duration_with_overhead, i64),
                            ("compute_units", cu, i64),
                        );
                        */
                        s.send((ss, h[0..10].into_iter().copied().collect::<Vec<_>>()))
                            .unwrap();
                        i += 1;
                    }
                })
                .unwrap()
        })
        .collect::<Vec<_>>();

    //joins.push(p);

    joins.push(pc);
    joins.into_iter().for_each(|j| j.join().unwrap());
}

struct ScheduleStage {
}

impl ScheduleStage {
    fn push_to_queue((weight, tx): (Weight, SanitizedTransaction), tx_queue: &mut TransactionQueue) {
        let mut rng = rand::thread_rng(); // manage randomness properly for future scheduling determinism
        //let ix = 23;
        //let tx = bank
        //    .verify_transaction(
        //        tx,
        //        solana_sdk::transaction::TransactionVerificationMode::FullVerification,
        //    )
        //    .unwrap();
        //tx.foo();
        tx_queue.add(
            UniqueWeight { weight, unique_key: solana_sdk::hash::new_rand(&mut rng) },
            Task { tx },
        );
    }

    fn pop_then_lock_from_queue(
        tx_queue: &mut TransactionQueue,
        address_book: &mut AddressBook,
    ) -> Option<(Task, Vec<LockAttempt>)> {
        for (unique_weight, next_task) in tx_queue.pop_next_task() {
            let message_hash = next_task.tx.message_hash();
            let locks = next_task.tx.get_account_locks().unwrap();

            // plumb message_hash into StatusCache or implmenent our own for duplicate tx
            // detection?

            let lock_attempts = attempt_lock_for_execution(address_book, &unique_weight, &message_hash, &locks);
            let is_success = lock_attempts.iter().all(|g| g.is_success());

            if is_success {
                return Some((next_task, lock_attempts));
            } else {
                ensure_unlock_for_failed_execution(address_book, lock_attempts);
                return None;
            }
        }

        None
    }

    fn create_execution_environment(task: Task, attemps: Vec<LockAttempt>) -> ExecutionEnvironment {
        // relock_before_execution() / update_address_book() / update_uncontended_addresses()?
        // load account now from AccountsDb
        panic!()
    }

    fn commit_result(ee: &mut ExecutionEnvironment, address_book: &mut AddressBook) {
        let lock_attempts = std::mem::take(&mut ee.lock_attempts);
        // do par()-ly?
        unlock_after_execution(address_book, lock_attempts);

        // par()-ly clone updated Accounts into address book
    }

    fn schedule_next_execution(
        tx_queue: &mut TransactionQueue,
        address_book: &mut AddressBook,
    ) -> Option<ExecutionEnvironment> {
        Self::pop_then_lock_from_queue(tx_queue, address_book).map(|(t, ll)| Self::create_execution_environment(t, ll))
    }

    fn register_runnable_task(weighted_tx: (Weight, SanitizedTransaction), tx_queue: &mut TransactionQueue) {
        Self::push_to_queue(weighted_tx, tx_queue)
    }

    fn run(
        tx_queue: &mut TransactionQueue,
        address_book: &mut AddressBook,
        bank: solana_runtime::bank::Bank,
        from_previous_stage: crossbeam_channel::Receiver<(Weight, SanitizedTransaction)>,
        to_execute_stage: crossbeam_channel::Sender<Option<ExecutionEnvironment>>, // ideally want to stop wrapping with Option<...>...
        from_execute_stage: crossbeam_channel::Receiver<ExecutionEnvironment>,
        to_next_stage: crossbeam_channel::Sender<ExecutionEnvironment>, // assume unbounded
    ) {
        use crossbeam_channel::select;
        let exit = true;
        while exit {
            select! {
                recv(from_previous_stage) -> weighted_tx => {
                    let weighted_tx = weighted_tx.unwrap();
                    Self::register_runnable_task(weighted_tx, tx_queue)
                }
                send(to_execute_stage, Self::schedule_next_execution(tx_queue, address_book)) -> res => {
                    res.unwrap();
                }
                recv(from_execute_stage) -> processed_execution_environment => {
                    let mut processed_execution_environment = processed_execution_environment.unwrap();

                    Self::commit_result(&mut processed_execution_environment, address_book);

                    // async-ly propagate the result to rpc subsystems
                    // to_next_stage is assumed to be non-blocking so, doesn't need to be one of select! handlers
                    to_next_stage.send(processed_execution_environment).unwrap()
                }
            }
        }
    }
}

struct ExecuteStage {
    //bank: Bank,
}

impl ExecuteStage {
}

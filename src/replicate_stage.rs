//! The `replicate_stage` replicates transactions broadcast by the leader.

use bank::Bank;
use cluster_info::ClusterInfo;
use counter::Counter;
use entry::EntryReceiver;
use leader_scheduler::LeaderScheduler;
use ledger::{Block, LedgerWriter};
use log::Level;
use result::{Error, Result};
use service::Service;
use signature::{Keypair, KeypairUtil};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, RwLock};
use std::thread::{self, Builder, JoinHandle};
use std::time::Duration;
use std::time::Instant;
use streamer::{responder, BlobSender};
use vote_stage::send_validator_vote;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ReplicateStageReturnType {
    LeaderRotation(u64),
}

// Implement a destructor for the ReplicateStage thread to signal it exited
// even on panics
struct Finalizer {
    exit_sender: Arc<AtomicBool>,
}

impl Finalizer {
    fn new(exit_sender: Arc<AtomicBool>) -> Self {
        Finalizer { exit_sender }
    }
}
// Implement a destructor for Finalizer.
impl Drop for Finalizer {
    fn drop(&mut self) {
        self.exit_sender.clone().store(true, Ordering::Relaxed);
    }
}

pub struct ReplicateStage {
    t_responder: JoinHandle<()>,
    t_replicate: JoinHandle<Option<ReplicateStageReturnType>>,
}

impl ReplicateStage {
    /// Process entry blobs, already in order
    fn replicate_requests(
        bank: &Arc<Bank>,
        cluster_info: &Arc<RwLock<ClusterInfo>>,
        window_receiver: &EntryReceiver,
        ledger_writer: Option<&mut LedgerWriter>,
        keypair: &Arc<Keypair>,
        vote_blob_sender: Option<&BlobSender>,
        entry_height: &mut u64,
        leader_scheduler_option: &mut Option<Arc<RwLock<LeaderScheduler>>>,
    ) -> Result<()> {
        let timer = Duration::new(1, 0);
        //coalesce all the available entries into a single vote
        let mut entries = window_receiver.recv_timeout(timer)?;
        while let Ok(mut more) = window_receiver.try_recv() {
            entries.append(&mut more);
        }

        {
            let mut leader_scheduler_lock_option = None;
            if let Some(leader_scheduler_lock) = leader_scheduler_option {
                let wlock = leader_scheduler_lock.write().unwrap();
                leader_scheduler_lock_option = Some(wlock);
            }

            let mut num_entries_to_write = entries.len();
            for (i, entry) in entries.iter().enumerate() {
                let res = bank.process_entry(
                    &entry,
                    Some(*entry_height + i as u64 + 1),
                    &mut leader_scheduler_lock_option
                        .as_mut()
                        .map(|wlock| &mut (**wlock)),
                );

                if let Some(ref leader_scheduler) = leader_scheduler_lock_option {
                    let my_id = keypair.pubkey();
                    match leader_scheduler.get_scheduled_leader(*entry_height + i as u64 + 1) {
                        // If we are the next leader, exit
                        Some(next_leader_id) if my_id == next_leader_id => {
                            num_entries_to_write = i + 1;
                            break;
                        }
                        None => panic!(
                            "Scheduled leader id should never be unknown while processing entries"
                        ),
                        _ => (),
                    }
                }

                if let Err(e) = res {
                    error!("{:?}", e)
                }
            }
            entries.truncate(num_entries_to_write);
        }

        if let Some(sender) = vote_blob_sender {
            send_validator_vote(bank, keypair, cluster_info, sender)?;
        }
        let votes = &entries.votes(*entry_height);
        wcluster_info.write().unwrap().insert_votes(votes);

        inc_new_counter_info!(
            "replicate-transactions",
            entries.iter().map(|x| x.transactions.len()).sum()
        );

        let entries_len = entries.len() as u64;
        // TODO: move this to another stage?
        if let Some(ledger_writer) = ledger_writer {
            ledger_writer.write_entries(entries)?;
        }

        *entry_height += entries_len;

        Ok(())
    }

    pub fn new(
        keypair: Arc<Keypair>,
        bank: Arc<Bank>,
        cluster_info: Arc<RwLock<ClusterInfo>>,
        window_receiver: EntryReceiver,
        ledger_path: Option<&str>,
        exit: Arc<AtomicBool>,
        entry_height: u64,
        leader_scheduler_option: Option<Arc<RwLock<LeaderScheduler>>>,
    ) -> Self {
        let (vote_blob_sender, vote_blob_receiver) = channel();
        let send = UdpSocket::bind("0.0.0.0:0").expect("bind");
        let t_responder = responder("replicate_stage", Arc::new(send), vote_blob_receiver);

        let mut ledger_writer = ledger_path.map(|p| LedgerWriter::open(p, false).unwrap());
        let keypair = Arc::new(keypair);

        let t_replicate = Builder::new()
            .name("solana-replicate-stage".to_string())
            .spawn(move || {
                let _exit = Finalizer::new(exit);
                let now = Instant::now();
                let mut next_vote_secs = 1;
                let mut entry_height_ = entry_height;
                let mut leader_scheduler_option_ = leader_scheduler_option;
                loop {
                    if let Some(ref leader_scheduler_lock) = leader_scheduler_option_ {
                        let leader_id = leader_scheduler_lock
                            .read()
                            .unwrap()
                            .get_scheduled_leader(entry_height_)
                            .expect("Scheduled leader id should never be unknown at this point");
                        if leader_id == keypair.pubkey() {
                            return Some(ReplicateStageReturnType::LeaderRotation(entry_height_));
                        }
                    }
                    // Only vote once a second.
                    let vote_sender = if now.elapsed().as_secs() > next_vote_secs {
                        next_vote_secs += 1;
                        Some(&vote_blob_sender)
                    } else {
                        None
                    };

                    if let Err(e) = Self::replicate_requests(
                        &bank,
                        &cluster_info,
                        &window_receiver,
                        ledger_writer.as_mut(),
                        &keypair,
                        vote_sender,
                        &mut entry_height_,
                        &mut leader_scheduler_option_,
                    ) {
                        match e {
                            Error::RecvTimeoutError(RecvTimeoutError::Disconnected) => break,
                            Error::RecvTimeoutError(RecvTimeoutError::Timeout) => (),
                            _ => error!("{:?}", e),
                        }
                    }
                }

                return None;
            }).unwrap();

        ReplicateStage {
            t_responder,
            t_replicate,
        }
    }
}

impl Service for ReplicateStage {
    type JoinReturnType = Option<ReplicateStageReturnType>;

    fn join(self) -> thread::Result<Option<ReplicateStageReturnType>> {
        self.t_responder.join()?;
        self.t_replicate.join()
    }
}

#[cfg(test)]
mod test {
    use crdt::{Crdt, Node};
    use fullnode::Fullnode;
    use leader_scheduler::{make_active_set_entries, LeaderScheduler, LeaderSchedulerConfig};
    use ledger::{genesis, next_entries_mut, LedgerWriter};
    use logger;
    use replicate_stage::{ReplicateStage, ReplicateStageReturnType};
    use service::Service;
    use signature::{Keypair, KeypairUtil};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::channel;
    use std::sync::{Arc, RwLock};

    #[test]
    pub fn test_replicate_stage_leader_rotation_exit() {
        logger::setup();

        // Set up dummy node to host a ReplicateStage
        let my_keypair = Keypair::new();
        let my_id = my_keypair.pubkey();
        let my_node = Node::new_localhost_with_pubkey(my_id);
        let crdt_me = Crdt::new(my_node.info.clone()).expect("Crdt::new");

        // Create a ledger
        let (mint, my_ledger_path) = genesis("test_replicate_stage_leader_rotation_exit", 10_000);
        let genesis_entries = mint.create_entries();
        let mut last_id = genesis_entries
            .last()
            .expect("expected at least one genesis entry")
            .id;

        // Write two entries to the ledger so that the validator is in the active set:
        // 1) Give him nonzero number of tokens 2) A vote from the validator .
        // This will cause leader rotation after the bootstrap height
        let mut ledger_writer = LedgerWriter::open(&my_ledger_path, false).unwrap();
        let bootstrap_entries = make_active_set_entries(&my_keypair, &mint.keypair(), &last_id);
        last_id = bootstrap_entries.last().unwrap().id;
        let ledger_initial_len = (genesis_entries.len() + bootstrap_entries.len()) as u64;
        ledger_writer.write_entries(bootstrap_entries).unwrap();

        // Set up the LeaderScheduler so that this this node becomes the leader at
        // bootstrap_height = num_bootstrap_epochs * leader_rotation_interval
        let old_leader_id = Keypair::new().pubkey();
        let leader_rotation_interval = 10;
        let num_bootstrap_epochs = 2;
        let bootstrap_height = num_bootstrap_epochs * leader_rotation_interval;
        let leader_scheduler_config = LeaderSchedulerConfig::new(
            old_leader_id,
            Some(bootstrap_height),
            Some(leader_rotation_interval),
            Some(leader_rotation_interval * 2),
            Some(bootstrap_height),
        );

        let mut leader_scheduler = LeaderScheduler::new(&leader_scheduler_config);

        // Set up the bank
        let (bank, _, _) =
            Fullnode::new_bank_from_ledger(&my_ledger_path, Some(&mut leader_scheduler));

        // Set up the replicate stage
        let (entry_sender, entry_receiver) = channel();
        let exit = Arc::new(AtomicBool::new(false));
        let replicate_stage = ReplicateStage::new(
            Arc::new(my_keypair),
            Arc::new(bank),
            Arc::new(RwLock::new(crdt_me)),
            entry_receiver,
            Some(&my_ledger_path),
            exit.clone(),
            ledger_initial_len,
            Some(Arc::new(RwLock::new(leader_scheduler))),
        );

        // Send enough entries to trigger leader rotation
        let extra_entries = leader_rotation_interval;
        let total_entries_to_send = (bootstrap_height + extra_entries) as usize;
        let mut num_hashes = 0;
        let mut entries_to_send = vec![];

        while entries_to_send.len() < total_entries_to_send {
            let entries = next_entries_mut(&mut last_id, &mut num_hashes, vec![]);
            last_id = entries.last().expect("expected at least one entry").id;
            entries_to_send.extend(entries);
        }

        entries_to_send.truncate(total_entries_to_send);
        entry_sender.send(entries_to_send).unwrap();

        // Wait for replicate_stage to exit and check return value is correct
        assert_eq!(
            Some(ReplicateStageReturnType::LeaderRotation(bootstrap_height)),
            replicate_stage.join().expect("replicate stage join")
        );

        assert_eq!(exit.load(Ordering::Relaxed), true);

        //Check ledger height is correct
        let (_, entry_height, _) = Fullnode::new_bank_from_ledger(&my_ledger_path, None);

        assert_eq!(entry_height, bootstrap_height);
    }
}

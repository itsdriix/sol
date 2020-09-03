use crate::bank::Bank;
use solana_sdk::{clock::Slot, commitment_config::CommitmentLevel};
use solana_vote_program::vote_state::MAX_LOCKOUT_HISTORY;
use std::{collections::HashMap, sync::Arc};

pub const VOTE_THRESHOLD_SIZE: f64 = 2f64 / 3f64;

pub type BlockCommitmentArray = [u64; MAX_LOCKOUT_HISTORY + 1];

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct BlockCommitment {
    pub commitment: BlockCommitmentArray,
}

impl BlockCommitment {
    pub fn increase_confirmation_stake(&mut self, confirmation_count: usize, stake: u64) {
        assert!(confirmation_count > 0 && confirmation_count <= MAX_LOCKOUT_HISTORY);
        self.commitment[confirmation_count - 1] += stake;
    }

    pub fn get_confirmation_stake(&mut self, confirmation_count: usize) -> u64 {
        assert!(confirmation_count > 0 && confirmation_count <= MAX_LOCKOUT_HISTORY);
        self.commitment[confirmation_count - 1]
    }

    pub fn increase_rooted_stake(&mut self, stake: u64) {
        self.commitment[MAX_LOCKOUT_HISTORY] += stake;
    }

    pub fn get_rooted_stake(&self) -> u64 {
        self.commitment[MAX_LOCKOUT_HISTORY]
    }

    pub fn new(commitment: BlockCommitmentArray) -> Self {
        Self { commitment }
    }
}

/// A node's view of cluster commitment as per a particular bank
#[derive(Default)]
pub struct BlockCommitmentCache {
    /// Map of all commitment levels of current ancestor slots, aggregated from the vote account
    /// data in the bank
    block_commitment: HashMap<Slot, BlockCommitment>,
    /// Cache slot details. Cluster data is calculated from the block_commitment map, and cached in
    /// the struct to avoid the expense of recalculating on every call.
    commitment_banks: CommitmentBanks,
    /// Total stake active during the bank's epoch
    total_stake: u64,
}

impl std::fmt::Debug for BlockCommitmentCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockCommitmentCache")
            .field("block_commitment", &self.block_commitment)
            .field("total_stake", &self.total_stake)
            .field(
                "bank",
                &format_args!("Bank({{current_slot: {:?}}})", self.commitment_banks.slot()),
            )
            .field("root", &self.commitment_banks.root_slot())
            .finish()
    }
}

impl BlockCommitmentCache {
    pub fn new(
        block_commitment: HashMap<Slot, BlockCommitment>,
        total_stake: u64,
        commitment_banks: CommitmentBanks,
    ) -> Self {
        Self {
            block_commitment,
            total_stake,
            commitment_banks,
        }
    }

    pub fn get_block_commitment(&self, slot: Slot) -> Option<&BlockCommitment> {
        self.block_commitment.get(&slot)
    }

    pub fn total_stake(&self) -> u64 {
        self.total_stake
    }

    pub fn slot(&self) -> Slot {
        self.commitment_banks.slot()
    }

    pub fn root(&self) -> Slot {
        self.commitment_banks.root_slot()
    }

    pub fn highest_confirmed_slot(&self) -> Slot {
        self.commitment_banks.highest_confirmed_slot()
    }

    pub fn highest_confirmed_root(&self) -> Slot {
        self.commitment_banks.highest_confirmed_root_slot()
    }

    pub fn commitment_slots(&self) -> CommitmentSlots {
        self.commitment_banks.commitment_slots()
    }

    pub fn highest_gossip_confirmed_slot(&self) -> Slot {
        // TODO: see solana_core::RpcSubscriptions:
        //self.last_checked_slots.get(&CommitmentLevel::SingleGossip).unwrap_or(&0)
        self.highest_confirmed_slot()
    }

    pub fn slot_with_commitment(&self, commitment_level: CommitmentLevel) -> Slot {
        match commitment_level {
            CommitmentLevel::Recent => self.slot(),
            CommitmentLevel::Root => self.root(),
            CommitmentLevel::Single => self.highest_confirmed_slot(),
            CommitmentLevel::SingleGossip => self.highest_gossip_confirmed_slot(),
            CommitmentLevel::Max => self.highest_confirmed_root(),
        }
    }

    fn highest_slot_with_confirmation_count(&self, confirmation_count: usize) -> Slot {
        assert!(confirmation_count > 0 && confirmation_count <= MAX_LOCKOUT_HISTORY);
        for slot in (self.root()..self.slot()).rev() {
            if let Some(count) = self.get_confirmation_count(slot) {
                if count >= confirmation_count {
                    return slot;
                }
            }
        }
        self.commitment_banks.root_slot()
    }

    pub fn calculate_highest_confirmed_slot(&self) -> Slot {
        self.highest_slot_with_confirmation_count(1)
    }

    pub fn get_confirmation_count(&self, slot: Slot) -> Option<usize> {
        self.get_lockout_count(slot, VOTE_THRESHOLD_SIZE)
    }

    // Returns the lowest level at which at least `minimum_stake_percentage` of the total epoch
    // stake is locked out
    fn get_lockout_count(&self, slot: Slot, minimum_stake_percentage: f64) -> Option<usize> {
        self.get_block_commitment(slot).map(|block_commitment| {
            let iterator = block_commitment.commitment.iter().enumerate().rev();
            let mut sum = 0;
            for (i, stake) in iterator {
                sum += stake;
                if (sum as f64 / self.total_stake as f64) > minimum_stake_percentage {
                    return i + 1;
                }
            }
            0
        })
    }

    pub fn new_for_tests() -> Self {
        let mut block_commitment: HashMap<Slot, BlockCommitment> = HashMap::new();
        block_commitment.insert(0, BlockCommitment::default());
        Self {
            block_commitment,
            total_stake: 42,
            ..Self::default()
        }
    }

    pub fn new_for_tests_with_slots(bank: Arc<Bank>, root: Arc<Bank>) -> Self {
        let mut block_commitment: HashMap<Slot, BlockCommitment> = HashMap::new();
        block_commitment.insert(0, BlockCommitment::default());
        Self {
            block_commitment,
            total_stake: 42,
            commitment_banks: CommitmentBanks {
                bank,
                root: Some(root.clone()),
                highest_confirmed_slot: Some(root.clone()),
                highest_confirmed_root: Some(root),
            },
        }
    }

    pub fn set_highest_confirmed_slot(&mut self, bank: Arc<Bank>) {
        self.commitment_banks.highest_confirmed_slot = Some(bank);
    }

    pub fn set_highest_confirmed_root(&mut self, root: Arc<Bank>) {
        self.commitment_banks.highest_confirmed_root = Some(root);
    }

    pub fn initialize_slots(&mut self, bank: Arc<Bank>) {
        self.commitment_banks.bank = bank.clone();
        self.commitment_banks.root = Some(bank);
    }
}

#[derive(Default, Clone, Copy)]
pub struct CommitmentSlots {
    /// The slot of the bank from which all other slots were calculated.
    pub slot: Slot,
    /// The current node root
    pub root: Slot,
    /// Highest cluster-confirmed slot
    pub highest_confirmed_slot: Slot,
    /// Highest cluster-confirmed root
    pub highest_confirmed_root: Slot,
}

impl CommitmentSlots {
    pub fn new_from_slot(slot: Slot) -> Self {
        Self {
            slot,
            ..Self::default()
        }
    }
}

#[derive(Default, Clone)]
pub struct CommitmentBanks {
    /// The slot of the bank from which all other slots were calculated.
    pub bank: Arc<Bank>,
    /// The current node root
    pub root: Option<Arc<Bank>>,
    /// Highest cluster-confirmed slot
    pub highest_confirmed_slot: Option<Arc<Bank>>,
    /// Highest cluster-confirmed root
    pub highest_confirmed_root: Option<Arc<Bank>>,
}

impl CommitmentBanks {
    pub fn new_from_bank(bank: Arc<Bank>) -> Self {
        Self {
            bank,
            ..Self::default()
        }
    }

    pub fn commitment_slots(&self) -> CommitmentSlots {
        CommitmentSlots {
            slot: self.slot(),
            root: self.root_slot(),
            highest_confirmed_slot: self.highest_confirmed_slot(),
            highest_confirmed_root: self.highest_confirmed_root_slot(),
        }
    }

    pub fn slot(&self) -> Slot {
        self.bank.slot()
    }

    pub fn root_slot(&self) -> Slot {
        self.root
            .as_ref()
            .map(|bank| bank.slot())
            .unwrap_or(u64::MAX)
    }

    pub fn highest_confirmed_slot(&self) -> Slot {
        self.highest_confirmed_slot
            .as_ref()
            .map(|bank| bank.slot())
            .unwrap_or(u64::MAX)
    }

    pub fn highest_confirmed_root_slot(&self) -> Slot {
        self.highest_confirmed_root
            .as_ref()
            .map(|bank| bank.slot())
            .unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_commitment() {
        let mut cache = BlockCommitment::default();
        assert_eq!(cache.get_confirmation_stake(1), 0);
        cache.increase_confirmation_stake(1, 10);
        assert_eq!(cache.get_confirmation_stake(1), 10);
        cache.increase_confirmation_stake(1, 20);
        assert_eq!(cache.get_confirmation_stake(1), 30);
    }

    #[test]
    fn test_get_confirmations() {
        // Build BlockCommitmentCache with votes at depths 0 and 1 for 2 slots
        let mut cache0 = BlockCommitment::default();
        cache0.increase_confirmation_stake(1, 5);
        cache0.increase_confirmation_stake(2, 40);

        let mut cache1 = BlockCommitment::default();
        cache1.increase_confirmation_stake(1, 40);
        cache1.increase_confirmation_stake(2, 5);

        let mut cache2 = BlockCommitment::default();
        cache2.increase_confirmation_stake(1, 20);
        cache2.increase_confirmation_stake(2, 5);

        let mut block_commitment = HashMap::new();
        block_commitment.entry(0).or_insert(cache0);
        block_commitment.entry(1).or_insert(cache1);
        block_commitment.entry(2).or_insert(cache2);
        let block_commitment_cache = BlockCommitmentCache {
            block_commitment,
            total_stake: 50,
            ..BlockCommitmentCache::default()
        };

        assert_eq!(block_commitment_cache.get_confirmation_count(0), Some(2));
        assert_eq!(block_commitment_cache.get_confirmation_count(1), Some(1));
        assert_eq!(block_commitment_cache.get_confirmation_count(2), Some(0),);
        assert_eq!(block_commitment_cache.get_confirmation_count(3), None,);
    }

    #[test]
    fn test_highest_confirmed_slot() {
        let bank_slot_5 = 5;
        let total_stake = 50;

        // Build cache with confirmation_count 2 given total_stake
        let mut cache0 = BlockCommitment::default();
        cache0.increase_confirmation_stake(1, 5);
        cache0.increase_confirmation_stake(2, 40);

        // Build cache with confirmation_count 1 given total_stake
        let mut cache1 = BlockCommitment::default();
        cache1.increase_confirmation_stake(1, 40);
        cache1.increase_confirmation_stake(2, 5);

        // Build cache with confirmation_count 0 given total_stake
        let mut cache2 = BlockCommitment::default();
        cache2.increase_confirmation_stake(1, 20);
        cache2.increase_confirmation_stake(2, 5);

        let mut block_commitment = HashMap::new();
        block_commitment.entry(1).or_insert_with(|| cache0.clone()); // Slot 1, conf 2
        block_commitment.entry(2).or_insert_with(|| cache1.clone()); // Slot 2, conf 1
        block_commitment.entry(3).or_insert_with(|| cache2.clone()); // Slot 3, conf 0
        let commitment_slots = CommitmentSlots::new_from_slot(bank_slot_5);
        let block_commitment_cache =
            BlockCommitmentCache::new(block_commitment, total_stake, commitment_slots);

        assert_eq!(block_commitment_cache.calculate_highest_confirmed_slot(), 2);

        // Build map with multiple slots at conf 1
        let mut block_commitment = HashMap::new();
        block_commitment.entry(1).or_insert_with(|| cache1.clone()); // Slot 1, conf 1
        block_commitment.entry(2).or_insert_with(|| cache1.clone()); // Slot 2, conf 1
        block_commitment.entry(3).or_insert_with(|| cache2.clone()); // Slot 3, conf 0
        let block_commitment_cache =
            BlockCommitmentCache::new(block_commitment, total_stake, commitment_slots);

        assert_eq!(block_commitment_cache.calculate_highest_confirmed_slot(), 2);

        // Build map with slot gaps
        let mut block_commitment = HashMap::new();
        block_commitment.entry(1).or_insert_with(|| cache1.clone()); // Slot 1, conf 1
        block_commitment.entry(3).or_insert(cache1); // Slot 3, conf 1
        block_commitment.entry(5).or_insert_with(|| cache2.clone()); // Slot 5, conf 0
        let block_commitment_cache =
            BlockCommitmentCache::new(block_commitment, total_stake, commitment_slots);

        assert_eq!(block_commitment_cache.calculate_highest_confirmed_slot(), 3);

        // Build map with no conf 1 slots, but one higher
        let mut block_commitment = HashMap::new();
        block_commitment.entry(1).or_insert(cache0); // Slot 1, conf 2
        block_commitment.entry(2).or_insert_with(|| cache2.clone()); // Slot 2, conf 0
        block_commitment.entry(3).or_insert_with(|| cache2.clone()); // Slot 3, conf 0
        let block_commitment_cache =
            BlockCommitmentCache::new(block_commitment, total_stake, commitment_slots);

        assert_eq!(block_commitment_cache.calculate_highest_confirmed_slot(), 1);

        // Build map with no conf 1 or higher slots
        let mut block_commitment = HashMap::new();
        block_commitment.entry(1).or_insert_with(|| cache2.clone()); // Slot 1, conf 0
        block_commitment.entry(2).or_insert_with(|| cache2.clone()); // Slot 2, conf 0
        block_commitment.entry(3).or_insert(cache2); // Slot 3, conf 0
        let block_commitment_cache =
            BlockCommitmentCache::new(block_commitment, total_stake, commitment_slots);

        assert_eq!(block_commitment_cache.calculate_highest_confirmed_slot(), 0);
    }
}

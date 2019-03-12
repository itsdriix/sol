//! The `bank_forks` module implments BankForks a DAG of checkpointed Banks

use hashbrown::{HashMap, HashSet};
use solana_runtime::bank::Bank;
use std::ops::Index;
use std::sync::Arc;

pub struct BankForks {
    banks: HashMap<u64, Arc<Bank>>,
    working_bank: Arc<Bank>,
}

impl Index<u64> for BankForks {
    type Output = Arc<Bank>;
    fn index(&self, bank_slot: u64) -> &Arc<Bank> {
        &self.banks[&bank_slot]
    }
}

impl BankForks {
    pub fn new(bank_slot: u64, bank: Bank) -> Self {
        let mut banks = HashMap::new();
        let working_bank = Arc::new(bank);
        banks.insert(bank_slot, working_bank.clone());
        Self {
            banks,
            working_bank,
        }
    }

    //produce a flat map of banks to all of its parents
    pub fn flat_parents(&self) -> HashMap<u64, HashSet<u64>> {
        let mut flat_parents = HashMap::new();
        let mut pending: Vec<Arc<Bank>> = self.banks.values().cloned().collect();
        while !pending.is_empty() {
            let bank = pending.pop().unwrap();
            if flat_parents.get(&bank.slot()).is_some() {
                continue;
            }
            let set = bank.parents().into_iter().map(|b| b.slot()).collect();
            flat_parents.insert(bank.slot(), set);
            pending.extend(bank.parents().into_iter());
        }
        flat_parents
    }

    //produce a flat map of banks to all of its children
    pub fn flat_children(&self) -> HashMap<u64, HashSet<u64>> {
        let mut flat_children = HashMap::new();
        let mut pending: Vec<Arc<Bank>> = self.banks.values().cloned().collect();
        let mut done = HashSet::new();
        assert!(!pending.is_empty());
        while !pending.is_empty() {
            let bank = pending.pop().unwrap();
            if done.contains(&bank.slot()) {
                continue;
            }
            done.insert(bank.slot());
            let _ = flat_children.entry(bank.slot()).or_insert(HashSet::new());
            for parent in bank.parents() {
                flat_children
                    .entry(parent.slot())
                    .or_insert(HashSet::new())
                    .insert(bank.slot());
            }
            pending.extend(bank.parents().into_iter());
        }
        flat_children
    }

    pub fn frozen_banks(&self) -> HashMap<u64, Arc<Bank>> {
        let mut frozen_banks: Vec<Arc<Bank>> = vec![];
        frozen_banks.extend(self.banks.values().filter(|v| v.is_frozen()).cloned());
        frozen_banks.extend(
            self.banks
                .iter()
                .flat_map(|(_, v)| v.parents())
                .filter(|v| v.is_frozen()),
        );
        frozen_banks.into_iter().map(|b| (b.slot(), b)).collect()
    }
    pub fn active_banks(&self) -> Vec<u64> {
        self.banks
            .iter()
            .filter(|(_, v)| !v.is_frozen())
            .map(|(k, _v)| *k)
            .collect()
    }
    pub fn get(&self, bank_slot: u64) -> Option<&Arc<Bank>> {
        self.banks.get(&bank_slot)
    }

    pub fn new_from_banks(initial_banks: &[Arc<Bank>]) -> Self {
        let mut banks = HashMap::new();
        let working_bank = initial_banks[0].clone();
        for bank in initial_banks {
            banks.insert(bank.slot(), bank.clone());
        }
        Self {
            banks,
            working_bank,
        }
    }

    // TODO: use the bank's own ID instead of receiving a parameter?
    pub fn insert(&mut self, bank_slot: u64, bank: Bank) {
        let mut bank = Arc::new(bank);
        assert_eq!(bank_slot, bank.slot());
        let prev = self.banks.insert(bank_slot, bank.clone());
        assert!(prev.is_none());

        self.working_bank = bank.clone();

        // TODO: this really only needs to look at the first
        //  parent if we're always calling insert()
        //  when we construct a child bank
        while let Some(parent) = bank.parent() {
            if let Some(prev) = self.banks.remove(&parent.slot()) {
                assert!(Arc::ptr_eq(&prev, &parent));
            }
            bank = parent;
        }
    }

    // TODO: really want to kill this...
    pub fn working_bank(&self) -> Arc<Bank> {
        self.working_bank.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::genesis_block::GenesisBlock;
    use solana_sdk::hash::Hash;
    use solana_sdk::pubkey::Pubkey;

    #[test]
    fn test_bank_forks() {
        let (genesis_block, _) = GenesisBlock::new(10_000);
        let bank = Bank::new(&genesis_block);
        let mut bank_forks = BankForks::new(0, bank);
        let child_bank = Bank::new_from_parent(&bank_forks[0u64], &Pubkey::default(), 1);
        child_bank.register_tick(&Hash::default());
        bank_forks.insert(1, child_bank);
        assert_eq!(bank_forks[1u64].tick_height(), 1);
        assert_eq!(bank_forks.working_bank().tick_height(), 1);
    }

    #[test]
    fn test_bank_forks_flat_children() {
        let (genesis_block, _) = GenesisBlock::new(10_000);
        let bank = Bank::new(&genesis_block);
        let mut bank_forks = BankForks::new(0, bank);
        let bank0 = bank_forks[0].clone();
        let bank = Bank::new_from_parent(&bank0, &Pubkey::default(), 1);
        bank_forks.insert(1, bank);
        let bank = Bank::new_from_parent(&bank0, &Pubkey::default(), 2);
        bank_forks.insert(2, bank);
        println!("here 4");
        let flat_children = bank_forks.flat_children();
        println!("here 5");
        let children: Vec<u64> = flat_children[&0].iter().cloned().collect();
        println!("here 6");
        assert_eq!(children, vec![1, 2]);
        println!("here 7");
        assert!(flat_children[&1].is_empty());
        assert!(flat_children[&2].is_empty());
    }

    #[test]
    fn test_bank_forks_flat_parents() {
        let (genesis_block, _) = GenesisBlock::new(10_000);
        let bank = Bank::new(&genesis_block);
        let mut bank_forks = BankForks::new(0, bank);
        let bank0 = bank_forks[0].clone();
        let bank = Bank::new_from_parent(&bank0, &Pubkey::default(), 1);
        bank_forks.insert(1, bank);
        let bank = Bank::new_from_parent(&bank0, &Pubkey::default(), 2);
        bank_forks.insert(2, bank);
        let flat_parents = bank_forks.flat_parents();
        assert!(flat_parents[&0].is_empty());
        let parents: Vec<u64> = flat_parents[&1].iter().cloned().collect();
        assert_eq!(parents, vec![0]);
        let parents: Vec<u64> = flat_parents[&2].iter().cloned().collect();
        assert_eq!(parents, vec![0]);
    }

    #[test]
    fn test_bank_forks_frozen_banks() {
        let (genesis_block, _) = GenesisBlock::new(10_000);
        let bank = Bank::new(&genesis_block);
        let mut bank_forks = BankForks::new(0, bank);
        let child_bank = Bank::new_from_parent(&bank_forks[0u64], &Pubkey::default(), 1);
        bank_forks.insert(1, child_bank);
        assert!(bank_forks.frozen_banks().get(&0).is_some());
        assert!(bank_forks.frozen_banks().get(&1).is_none());
    }

    #[test]
    fn test_bank_forks_active_banks() {
        let (genesis_block, _) = GenesisBlock::new(10_000);
        let bank = Bank::new(&genesis_block);
        let mut bank_forks = BankForks::new(0, bank);
        let child_bank = Bank::new_from_parent(&bank_forks[0u64], &Pubkey::default(), 1);
        bank_forks.insert(1, child_bank);
        assert_eq!(bank_forks.active_banks(), vec![1]);
    }

}

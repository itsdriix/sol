#![allow(clippy::arithmetic_side_effects)]

//#[global_allocator]
//static GLOBAL: bump_allocator::BumpPointer = bump_allocator::BumpPointer;
//
#[thread_local]
pub static mut LOCAL_ALLOCATOR: BL = BL::new();

struct BL {
    bytes: [u8; 10_000_000];
}

impl BL {
    const fn new() -> Self {
        Self {
            bytes: [0; 10_000_000],
        }
    }
}


use {
    assert_matches::assert_matches,
    iai_callgrind::{
        client_requests::callgrind::toggle_collect, library_benchmark, library_benchmark_group,
        main,
    },
    solana_sdk::{
        instruction::{AccountMeta, Instruction},
        message::Message,
        pubkey::Pubkey,
        signature::Signer,
        signer::keypair::Keypair,
        transaction::{SanitizedTransaction, Transaction},
    },
    solana_unified_scheduler_logic::{Page, SchedulingStateMachine},
};

#[library_benchmark]
#[bench::min(0)]
#[bench::one(1)]
#[bench::two(2)]
#[bench::three(3)]
#[bench::normal(32)]
#[bench::large(64)]
#[bench::max(128)]
fn bench_schedule_task(account_count: usize) {
    toggle_collect();
    let mut accounts = vec![];
    for i in 0..account_count {
        if i % 2 == 0 {
            accounts.push(AccountMeta::new(Keypair::new().pubkey(), true));
        } else {
            accounts.push(AccountMeta::new_readonly(Keypair::new().pubkey(), true));
        }
    }

    let payer = Keypair::new();
    let memo_ix = Instruction {
        program_id: Pubkey::default(),
        accounts,
        data: vec![0x00],
    };
    let mut ixs = vec![];
    for _ in 0..1 {
        ixs.push(memo_ix.clone());
    }
    let msg = Message::new(&ixs, Some(&payer.pubkey()));
    let txn = Transaction::new_unsigned(msg);
    //panic!("{:?}", txn);
    //assert_eq!(wire_txn.len(), 3);
    let tx0 = SanitizedTransaction::from_transaction_for_tests(txn);
    let task = SchedulingStateMachine::create_task(tx0, 0, &mut |_| Page::default());
    let mut scheduler = SchedulingStateMachine::default();
    toggle_collect();
    scheduler
        .schedule_task(task, |_task| {
            toggle_collect();
        })
        .unwrap();
}

#[library_benchmark]
#[bench::min(0)]
#[bench::one(1)]
#[bench::two(2)]
#[bench::three(3)]
#[bench::normal(32)]
#[bench::large(64)]
#[bench::max(128)]
fn bench_drop_task(account_count: usize) {
    toggle_collect();
    let mut accounts = vec![];
    for _ in 0..account_count {
        accounts.push(AccountMeta::new(Keypair::new().pubkey(), true));
    }

    let payer = Keypair::new();
    let memo_ix = Instruction {
        program_id: Pubkey::default(),
        accounts,
        data: vec![0x00],
    };
    let mut ixs = vec![];
    for _ in 0..1 {
        ixs.push(memo_ix.clone());
    }
    let msg = Message::new(&ixs, Some(&payer.pubkey()));
    let txn = Transaction::new_unsigned(msg);
    //panic!("{:?}", txn);
    //assert_eq!(wire_txn.len(), 3);
    let tx0 = SanitizedTransaction::from_transaction_for_tests(txn);
    let task = SchedulingStateMachine::create_task(tx0, 0, &mut |_| Page::default());

    toggle_collect();
    drop(task);
    toggle_collect();
}

#[library_benchmark]
#[bench::one(1)]
fn bench_insert_task(account_count: usize) {
    toggle_collect();
    let mut accounts = vec![];
    for _ in 0..account_count {
        accounts.push(AccountMeta::new(Keypair::new().pubkey(), true));
    }

    let payer = Keypair::new();
    let memo_ix = Instruction {
        program_id: Pubkey::default(),
        accounts,
        data: vec![0x00],
    };
    let mut ixs = vec![];
    for _ in 0..1 {
        ixs.push(memo_ix.clone());
    }
    let msg = Message::new(&ixs, Some(&payer.pubkey()));
    let txn = Transaction::new_unsigned(msg);
    //panic!("{:?}", txn);
    //assert_eq!(wire_txn.len(), 3);
    let tx0 = SanitizedTransaction::from_transaction_for_tests(txn);
    let task = SchedulingStateMachine::create_task(tx0, 0, &mut |_| Page::default());

    let mut b = std::collections::BTreeMap::new();
    toggle_collect();
    b.insert(task.unique_weight, task.clone());
    b.insert(task.unique_weight + 1, task.clone());
    b.remove(&task.unique_weight);
    b.remove(&(task.unique_weight + 1));
    //b.insert(task.unique_weight + 4, task);
    toggle_collect();
    drop(b);
}

#[library_benchmark]
#[bench::one(1)]
fn bench_heaviest_task(account_count: usize) {
    toggle_collect();
    let mut accounts = vec![];
    for _ in 0..account_count {
        accounts.push(AccountMeta::new(Keypair::new().pubkey(), true));
    }

    let payer = Keypair::new();
    let memo_ix = Instruction {
        program_id: Pubkey::default(),
        accounts,
        data: vec![0x00],
    };
    let mut ixs = vec![];
    for _ in 0..1 {
        ixs.push(memo_ix.clone());
    }
    let msg = Message::new(&ixs, Some(&payer.pubkey()));
    let txn = Transaction::new_unsigned(msg);
    //panic!("{:?}", txn);
    //assert_eq!(wire_txn.len(), 3);
    let tx0 = SanitizedTransaction::from_transaction_for_tests(txn);
    let task = SchedulingStateMachine::create_task(tx0, 0, &mut |_| Page::default());

    let mut b = std::collections::BTreeMap::new();
    b.insert(task.unique_weight, task.clone());
    b.insert(task.unique_weight + 1, task.clone());
    b.insert(task.unique_weight + 2, task.clone());
    let mut c = std::collections::BTreeMap::new();
    c.insert(task.unique_weight + 3, task.clone());
    c.insert(task.unique_weight + 4, task.clone());
    c.insert(task.unique_weight + 5, task.clone());

    toggle_collect();
    let d = b.first_key_value();
    let e = c.first_key_value();
    let f = std::cmp::min_by(d, e, |x, y| x.map(|x| x.0).cmp(&y.map(|y| y.0))).map(|x| x.1);
    assert_matches!(f.map(|f| f.task_index()), Some(0));
    toggle_collect();
    dbg!(f);

    drop(b);
}

#[library_benchmark]
#[bench::min(0)]
#[bench::one(1)]
#[bench::two(2)]
#[bench::three(3)]
#[bench::normal(32)]
#[bench::large(64)]
#[bench::max(128)]
fn bench_schedule_task_conflicting(account_count: usize) {
    toggle_collect();
    let mut accounts = vec![];
    for _ in 0..account_count {
        accounts.push(AccountMeta::new(Keypair::new().pubkey(), true));
    }

    let payer = Keypair::new();
    let memo_ix = Instruction {
        program_id: Pubkey::default(),
        accounts,
        data: vec![0x00],
    };
    let mut ixs = vec![];
    for _ in 0..1 {
        ixs.push(memo_ix.clone());
    }
    let msg = Message::new(&ixs, Some(&payer.pubkey()));
    let txn = Transaction::new_unsigned(msg);
    //panic!("{:?}", txn);
    //assert_eq!(wire_txn.len(), 3);
    let tx0 = SanitizedTransaction::from_transaction_for_tests(txn);
    let task = SchedulingStateMachine::create_task(tx0, 0, &mut |_| Page::default());
    let mut scheduler = SchedulingStateMachine::default();
    let task = scheduler.schedule_task_for_test(task).unwrap();
    let task2 = task.clone();
    toggle_collect();
    assert_matches!(scheduler.schedule_task_for_test(task2), None);
    toggle_collect();
    drop(task);
}

#[library_benchmark]
#[bench::min(3, 0)]
#[bench::one(3, 1)]
#[bench::two(2, 2)]
#[bench::three(3, 3)]
#[bench::normal(3, 32)]
#[bench::large(3, 64)]
#[bench::large2(3, 128)]
#[bench::large3(3, 256)]
#[bench::large4(3, 1024)]
#[bench::large5(3, 2048)]
fn bench_schedule_task_conflicting_hot(account_count: usize, task_count: usize) {
    toggle_collect();
    let mut accounts = vec![];
    for _ in 0..account_count {
        accounts.push(AccountMeta::new(Keypair::new().pubkey(), true));
    }

    let payer = Keypair::new();
    let memo_ix = Instruction {
        program_id: Pubkey::default(),
        accounts,
        data: vec![0x00],
    };
    let mut ixs = vec![];
    for _ in 0..1 {
        ixs.push(memo_ix.clone());
    }
    let msg = Message::new(&ixs, Some(&payer.pubkey()));
    let txn = Transaction::new_unsigned(msg);
    //panic!("{:?}", txn);
    //assert_eq!(wire_txn.len(), 3);
    let tx0 = SanitizedTransaction::from_transaction_for_tests(txn);

    let mut scheduler = SchedulingStateMachine::default();

    let mut pages: std::collections::HashMap<solana_sdk::pubkey::Pubkey, Page> =
        std::collections::HashMap::new();
    let task = SchedulingStateMachine::create_task(tx0.clone(), 0, &mut |address| {
        pages.entry(address).or_default().clone()
    });
    scheduler.schedule_task_for_test(task).unwrap();
    for i in 1..=task_count {
        let task = SchedulingStateMachine::create_task(tx0.clone(), i, &mut |address| {
            pages.entry(address).or_default().clone()
        });
        assert_matches!(scheduler.schedule_task_for_test(task), None);
    }

    let task = SchedulingStateMachine::create_task(tx0.clone(), task_count + 1, &mut |address| {
        pages.entry(address).or_default().clone()
    });
    let task2 = task.clone();

    toggle_collect();
    assert_matches!(scheduler.schedule_task_for_test(task2), None);
    toggle_collect();

    drop(task);
}

#[library_benchmark]
#[bench::min(0)]
#[bench::one(1)]
#[bench::two(2)]
#[bench::three(3)]
#[bench::normal(32)]
#[bench::large(64)]
#[bench::max(128)]
fn bench_deschedule_task_conflicting(account_count: usize) {
    toggle_collect();
    let mut accounts = vec![];
    for _ in 0..account_count {
        accounts.push(AccountMeta::new(Keypair::new().pubkey(), true));
    }

    let payer = Keypair::new();
    let memo_ix = Instruction {
        program_id: Pubkey::default(),
        accounts,
        data: vec![0x00],
    };
    let mut ixs = vec![];
    for _ in 0..1 {
        ixs.push(memo_ix.clone());
    }
    let msg = Message::new(&ixs, Some(&payer.pubkey()));
    let txn = Transaction::new_unsigned(msg);
    //panic!("{:?}", txn);
    //assert_eq!(wire_txn.len(), 3);
    let tx0 = SanitizedTransaction::from_transaction_for_tests(txn);
    let task = SchedulingStateMachine::create_task(tx0, 0, &mut |_| Page::default());
    let mut scheduler = SchedulingStateMachine::default();
    let task = scheduler.schedule_task_for_test(task).unwrap();
    assert_matches!(scheduler.schedule_task_for_test(task.clone()), None);

    toggle_collect();
    scheduler.deschedule_task(&task);
    toggle_collect();

    drop(task);
}

#[library_benchmark]
#[bench::min(0)]
#[bench::one(1)]
#[bench::two(2)]
#[bench::three(3)]
#[bench::normal(32)]
#[bench::large(64)]
#[bench::max(128)]
fn bench_schedule_retryable_task(account_count: usize) {
    toggle_collect();
    let mut accounts = vec![];
    for _ in 0..account_count {
        accounts.push(AccountMeta::new(Keypair::new().pubkey(), true));
    }

    let payer = Keypair::new();
    let memo_ix = Instruction {
        program_id: Pubkey::default(),
        accounts,
        data: vec![0x00],
    };
    let mut ixs = vec![];
    for _ in 0..1 {
        ixs.push(memo_ix.clone());
    }
    let msg = Message::new(&ixs, Some(&payer.pubkey()));
    let txn = Transaction::new_unsigned(msg);
    //panic!("{:?}", txn);
    //assert_eq!(wire_txn.len(), 3);
    let tx0 = SanitizedTransaction::from_transaction_for_tests(txn);
    let mut pages: std::collections::HashMap<solana_sdk::pubkey::Pubkey, Page> =
        std::collections::HashMap::new();
    let task = SchedulingStateMachine::create_task(tx0.clone(), 0, &mut |address| {
        pages.entry(address).or_default().clone()
    });
    let task2 = SchedulingStateMachine::create_task(tx0, 1, &mut |address| {
        pages.entry(address).or_default().clone()
    });
    let mut scheduler = SchedulingStateMachine::default();
    let task = scheduler.schedule_task_for_test(task).unwrap();
    assert_matches!(scheduler.schedule_task_for_test(task2), None);
    scheduler.deschedule_task(&task);
    toggle_collect();
    let retried_task = scheduler
        .schedule_retryable_task(|task| {
            toggle_collect();
            task.clone()
        })
        .unwrap();
    assert_eq!(task.transaction(), retried_task.transaction());
    drop(task);
}

#[library_benchmark]
#[bench::two(2)]
#[bench::three(3)]
#[bench::normal(32)]
#[bench::large(64)]
#[bench::max(128)]
fn bench_end_to_end_worst(account_count: usize) {
    toggle_collect();
    let mut accounts = vec![];
    for _ in 0..account_count {
        accounts.push(AccountMeta::new(Keypair::new().pubkey(), true));
    }

    let payer = Keypair::new();
    let memo_ix = Instruction {
        program_id: Pubkey::default(),
        accounts,
        data: vec![0x00],
    };
    let mut ixs = vec![];
    for _ in 0..1 {
        ixs.push(memo_ix.clone());
    }
    let msg = Message::new(&ixs, Some(&payer.pubkey()));
    let txn = Transaction::new_unsigned(msg);
    //panic!("{:?}", txn);
    //assert_eq!(wire_txn.len(), 3);
    let tx0 = SanitizedTransaction::from_transaction_for_tests(txn);
    let mut pages: std::collections::HashMap<solana_sdk::pubkey::Pubkey, Page> =
        std::collections::HashMap::new();
    let task = SchedulingStateMachine::create_task(tx0.clone(), 0, &mut |address| {
        pages.entry(address).or_default().clone()
    });
    let mut scheduler = SchedulingStateMachine::default();

    let task = scheduler.schedule_task_for_test(task).unwrap();
    for i in 1..account_count {
        let mut accounts = vec![memo_ix.accounts[i].clone()];
        for _ in 0..account_count {
            accounts.push(AccountMeta::new(Keypair::new().pubkey(), true));
        }

        let payer = Keypair::new();
        let memo_ix = Instruction {
            program_id: Pubkey::default(),
            accounts,
            data: vec![0x00],
        };
        let ixs = vec![memo_ix];
        let msg = Message::new(&ixs, Some(&payer.pubkey()));
        let txn = Transaction::new_unsigned(msg);
        //panic!("{:?}", txn);
        //assert_eq!(wire_txn.len(), 3);
        let tx0 = SanitizedTransaction::from_transaction_for_tests(txn);
        let task2 = SchedulingStateMachine::create_task(tx0, i, &mut |address| {
            pages.entry(address).or_default().clone()
        });
        assert_matches!(scheduler.schedule_task_for_test(task2.clone()), None);
    }

    toggle_collect();
    scheduler.deschedule_task(&task);
    if let Some(cc) = account_count.checked_sub(1) {
        assert_eq!(scheduler.retryable_task_count(), cc);
        let mut c = 0;
        while let Some(retried_task) = scheduler.schedule_retryable_task_for_test() {
            c += 1;
            scheduler.deschedule_task(&retried_task);
        }
        assert_eq!(c, cc);
    }
    toggle_collect();

    //assert_eq!(task2.task_index(), retried_task.task_index());
    drop(task);
}

#[library_benchmark]
#[bench::min(0)]
#[bench::one(1)]
#[bench::two(2)]
#[bench::three(3)]
#[bench::normal(32)]
#[bench::large(64)]
#[bench::max(128)]
fn bench_deschedule_task(account_count: usize) {
    toggle_collect();
    let mut accounts = vec![];
    for i in 0..account_count {
        if i % 2 == 0 {
            accounts.push(AccountMeta::new(Keypair::new().pubkey(), true));
        } else {
            accounts.push(AccountMeta::new_readonly(Keypair::new().pubkey(), true));
        }
    }

    let payer = Keypair::new();
    let memo_ix = Instruction {
        program_id: Pubkey::default(),
        accounts,
        data: vec![0x00],
    };
    let mut ixs = vec![];
    for _ in 0..1 {
        ixs.push(memo_ix.clone());
    }
    let msg = Message::new(&ixs, Some(&payer.pubkey()));
    let txn = Transaction::new_unsigned(msg);
    //panic!("{:?}", txn);
    //assert_eq!(wire_txn.len(), 3);
    let tx0 = SanitizedTransaction::from_transaction_for_tests(txn);
    let task = SchedulingStateMachine::create_task(tx0, 0, &mut |_| Page::default());
    let mut scheduler = SchedulingStateMachine::default();
    let task = scheduler.schedule_task_for_test(task).unwrap();
    toggle_collect();
    scheduler.deschedule_task(&task);
    toggle_collect();
    drop(task);
}

library_benchmark_group!(
    name = bench_scheduling_state_machine;
    //benchmarks = bench_drop_task, bench_insert_task, bench_heaviest_task, bench_schedule_task, bench_schedule_task_conflicting, bench_schedule_task_conflicting_hot, bench_deschedule_task, bench_deschedule_task_conflicting, bench_schedule_retryable_task
    benchmarks = bench_end_to_end_worst
);

main!(library_benchmark_groups = bench_scheduling_state_machine);

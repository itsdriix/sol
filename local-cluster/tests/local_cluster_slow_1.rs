//! If a test takes over 100s to run on CI, move it here so that it's clear where the
//! biggest improvements to CI times can be found.
#![allow(clippy::integer_arithmetic)]
use {
    common::*,
    log::*,
    serial_test::serial,
    solana_core::{
        broadcast_stage::{
            broadcast_duplicates_run::{BroadcastDuplicatesConfig, ClusterPartition},
            BroadcastStageType,
        },
        consensus::SWITCH_FORK_THRESHOLD,
        replay_stage::DUPLICATE_THRESHOLD,
        validator::ValidatorConfig,
    },
    solana_gossip::gossip_service::discover_cluster,
    solana_ledger::{
        ancestor_iterator::AncestorIterator, leader_schedule::FixedSchedule, shred::Shred,
    },
    solana_local_cluster::{
        cluster::{Cluster, ClusterValidatorInfo},
        cluster_tests::{self},
        local_cluster::{ClusterConfig, LocalCluster},
        validator_configs::*,
    },
    solana_runtime::vote_parser,
    solana_sdk::{
        clock::{Slot, MAX_PROCESSING_AGE},
        hash::Hash,
        pubkey::Pubkey,
        signature::Signer,
        vote::state::VoteStateUpdate,
    },
    solana_streamer::socket::SocketAddrSpace,
    solana_vote_program::{vote_state::MAX_LOCKOUT_HISTORY, vote_transaction},
    std::{
        collections::{BTreeSet, HashSet},
        net::UdpSocket,
        path::Path,
        thread::sleep,
        time::Duration,
    },
};

mod common;

#[test]
#[serial]
#[ignore]
// Steps in this test:
// We want to create a situation like:
/*
      1 (2%, killed and restarted) --- 200 (37%, lighter fork)
    /
    0
    \-------- 4 (38%, heavier fork)
*/
// where the 2% that voted on slot 1 don't see their votes land in a block
// due to blockhash expiration, and thus without resigning their votes with
// a newer blockhash, will deem slot 4 the heavier fork and try to switch to
// slot 4, which doesn't pass the switch threshold. This stalls the network.

// We do this by:
// 1) Creating a partition so all three nodes don't see each other
// 2) Kill the validator with 2%
// 3) Wait for longer than blockhash expiration
// 4) Copy in the lighter fork's blocks up, *only* up to the first slot in the lighter fork
// (not all the blocks on the lighter fork!), call this slot `L`
// 5) Restart the validator with 2% so that he votes on `L`, but the vote doesn't land
// due to blockhash expiration
// 6) Resolve the partition so that the 2% repairs the other fork, and tries to switch,
// stalling the network.

fn test_fork_choice_refresh_old_votes() {
    solana_logger::setup_with_default(RUST_LOG_FILTER);
    let max_switch_threshold_failure_pct = 1.0 - 2.0 * SWITCH_FORK_THRESHOLD;
    let total_stake = 100 * DEFAULT_NODE_STAKE;
    let max_failures_stake = (max_switch_threshold_failure_pct * total_stake as f64) as u64;

    // 1% less than the failure stake, where the 2% is allocated to a validator that
    // has no leader slots and thus won't be able to vote on its own fork.
    let failures_stake = max_failures_stake;
    let total_alive_stake = total_stake - failures_stake;
    let alive_stake_1 = total_alive_stake / 2 - 1;
    let alive_stake_2 = total_alive_stake - alive_stake_1 - 1;

    // Heavier fork still doesn't have enough stake to switch. Both branches need
    // the vote to land from the validator with `alive_stake_3` to allow the other
    // fork to switch.
    let alive_stake_3 = 2 * DEFAULT_NODE_STAKE;
    assert!(alive_stake_1 < alive_stake_2);
    assert!(alive_stake_1 + alive_stake_3 > alive_stake_2);

    let partitions: &[(usize, usize)] = &[
        (alive_stake_1 as usize, 8),
        (alive_stake_2 as usize, 8),
        (alive_stake_3 as usize, 0),
    ];

    #[derive(Default)]
    struct PartitionContext {
        alive_stake3_info: Option<ClusterValidatorInfo>,
        smallest_validator_key: Pubkey,
        lighter_fork_validator_key: Pubkey,
        heaviest_validator_key: Pubkey,
    }
    let on_partition_start = |cluster: &mut LocalCluster,
                              validator_keys: &[Pubkey],
                              _: Vec<ClusterValidatorInfo>,
                              context: &mut PartitionContext| {
        // Kill validator with alive_stake_3, second in `partitions` slice
        let smallest_validator_key = &validator_keys[3];
        let info = cluster.exit_node(smallest_validator_key);
        context.alive_stake3_info = Some(info);
        context.smallest_validator_key = *smallest_validator_key;
        // validator_keys[0] is the validator that will be killed, i.e. the validator with
        // stake == `failures_stake`
        context.lighter_fork_validator_key = validator_keys[1];
        // Third in `partitions` slice
        context.heaviest_validator_key = validator_keys[2];
    };

    let ticks_per_slot = 8;
    let on_before_partition_resolved =
        |cluster: &mut LocalCluster, context: &mut PartitionContext| {
            // Equal to ms_per_slot * MAX_PROCESSING_AGE, rounded up
            let sleep_time_ms = ms_for_n_slots(MAX_PROCESSING_AGE as u64, ticks_per_slot);
            info!("Wait for blockhashes to expire, {} ms", sleep_time_ms);

            // Wait for blockhashes to expire
            sleep(Duration::from_millis(sleep_time_ms));

            let smallest_ledger_path = context
                .alive_stake3_info
                .as_ref()
                .unwrap()
                .info
                .ledger_path
                .clone();
            let lighter_fork_ledger_path = cluster.ledger_path(&context.lighter_fork_validator_key);
            let heaviest_ledger_path = cluster.ledger_path(&context.heaviest_validator_key);

            // Get latest votes. We make sure to wait until the vote has landed in
            // blockstore. This is important because if we were the leader for the block there
            // is a possibility of voting before broadcast has inserted in blockstore.
            let lighter_fork_latest_vote = wait_for_last_vote_in_tower_to_land_in_ledger(
                &lighter_fork_ledger_path,
                &context.lighter_fork_validator_key,
            );
            let heaviest_fork_latest_vote = wait_for_last_vote_in_tower_to_land_in_ledger(
                &heaviest_ledger_path,
                &context.heaviest_validator_key,
            );

            // Open ledgers
            let smallest_blockstore = open_blockstore(&smallest_ledger_path);
            let lighter_fork_blockstore = open_blockstore(&lighter_fork_ledger_path);
            let heaviest_blockstore = open_blockstore(&heaviest_ledger_path);

            info!("Opened blockstores");

            // Find the first slot on the smaller fork
            let lighter_ancestors: BTreeSet<Slot> = std::iter::once(lighter_fork_latest_vote)
                .chain(AncestorIterator::new(
                    lighter_fork_latest_vote,
                    &lighter_fork_blockstore,
                ))
                .collect();
            let heavier_ancestors: BTreeSet<Slot> = std::iter::once(heaviest_fork_latest_vote)
                .chain(AncestorIterator::new(
                    heaviest_fork_latest_vote,
                    &heaviest_blockstore,
                ))
                .collect();
            let first_slot_in_lighter_partition = *lighter_ancestors
                .iter()
                .zip(heavier_ancestors.iter())
                .find(|(x, y)| x != y)
                .unwrap()
                .0;

            // Must have been updated in the above loop
            assert!(first_slot_in_lighter_partition != 0);
            info!(
                "First slot in lighter partition is {}",
                first_slot_in_lighter_partition
            );

            // Copy all the blocks from the smaller partition up to `first_slot_in_lighter_partition`
            // into the smallest validator's blockstore
            copy_blocks(
                first_slot_in_lighter_partition,
                &lighter_fork_blockstore,
                &smallest_blockstore,
            );

            // Restart the smallest validator that we killed earlier in `on_partition_start()`
            drop(smallest_blockstore);
            cluster.restart_node(
                &context.smallest_validator_key,
                context.alive_stake3_info.take().unwrap(),
                SocketAddrSpace::Unspecified,
            );

            loop {
                // Wait for node to vote on the first slot on the less heavy fork, so it'll need
                // a switch proof to flip to the other fork.
                // However, this vote won't land because it's using an expired blockhash. The
                // fork structure will look something like this after the vote:
                /*
                     1 (2%, killed and restarted) --- 200 (37%, lighter fork)
                    /
                    0
                    \-------- 4 (38%, heavier fork)
                */
                if let Some((last_vote_slot, _last_vote_hash)) =
                    last_vote_in_tower(&smallest_ledger_path, &context.smallest_validator_key)
                {
                    // Check that the heaviest validator on the other fork doesn't have this slot,
                    // this must mean we voted on a unique slot on this fork
                    if last_vote_slot == first_slot_in_lighter_partition {
                        info!(
                            "Saw vote on first slot in lighter partition {}",
                            first_slot_in_lighter_partition
                        );
                        break;
                    } else {
                        info!(
                            "Haven't seen vote on first slot in lighter partition, latest vote is: {}",
                            last_vote_slot
                        );
                    }
                }

                sleep(Duration::from_millis(20));
            }

            // Now resolve partition, allow validator to see the fork with the heavier validator,
            // but the fork it's currently on is the heaviest, if only its own vote landed!
        };

    // Check that new roots were set after the partition resolves (gives time
    // for lockouts built during partition to resolve and gives validators an opportunity
    // to try and switch forks)
    let on_partition_resolved = |cluster: &mut LocalCluster, _: &mut PartitionContext| {
        cluster.check_for_new_roots(16, "PARTITION_TEST", SocketAddrSpace::Unspecified);
    };

    run_kill_partition_switch_threshold(
        &[(failures_stake as usize - 1, 16)],
        partitions,
        Some(ticks_per_slot),
        PartitionContext::default(),
        on_partition_start,
        on_before_partition_resolved,
        on_partition_resolved,
    );
}

#[test]
#[serial]
fn test_kill_heaviest_partition() {
    // This test:
    // 1) Spins up four partitions, the heaviest being the first with more stake
    // 2) Schedules the other validators for sufficient slots in the schedule
    // so that they will still be locked out of voting for the major partition
    // when the partition resolves
    // 3) Kills the most staked partition. Validators are locked out, but should all
    // eventually choose the major partition
    // 4) Check for recovery
    let num_slots_per_validator = 8;
    let partitions: [usize; 4] = [
        11 * DEFAULT_NODE_STAKE as usize,
        10 * DEFAULT_NODE_STAKE as usize,
        10 * DEFAULT_NODE_STAKE as usize,
        10 * DEFAULT_NODE_STAKE as usize,
    ];
    let (leader_schedule, validator_keys) = create_custom_leader_schedule_with_random_keys(&[
        num_slots_per_validator * (partitions.len() - 1),
        num_slots_per_validator,
        num_slots_per_validator,
        num_slots_per_validator,
    ]);

    let empty = |_: &mut LocalCluster, _: &mut ()| {};
    let validator_to_kill = validator_keys[0].pubkey();
    let on_partition_resolved = |cluster: &mut LocalCluster, _: &mut ()| {
        info!("Killing validator with id: {}", validator_to_kill);
        cluster.exit_node(&validator_to_kill);
        cluster.check_for_new_roots(16, "PARTITION_TEST", SocketAddrSpace::Unspecified);
    };
    run_cluster_partition(
        &partitions,
        Some((leader_schedule, validator_keys)),
        (),
        empty,
        empty,
        on_partition_resolved,
        None,
        vec![],
    )
}

#[test]
#[serial]
fn test_kill_partition_switch_threshold_no_progress() {
    let max_switch_threshold_failure_pct = 1.0 - 2.0 * SWITCH_FORK_THRESHOLD;
    let total_stake = 10_000 * DEFAULT_NODE_STAKE;
    let max_failures_stake = (max_switch_threshold_failure_pct * total_stake as f64) as u64;

    let failures_stake = max_failures_stake;
    let total_alive_stake = total_stake - failures_stake;
    let alive_stake_1 = total_alive_stake / 2;
    let alive_stake_2 = total_alive_stake - alive_stake_1;

    // Check that no new roots were set 400 slots after partition resolves (gives time
    // for lockouts built during partition to resolve and gives validators an opportunity
    // to try and switch forks)
    let on_partition_start =
        |_: &mut LocalCluster, _: &[Pubkey], _: Vec<ClusterValidatorInfo>, _: &mut ()| {};
    let on_before_partition_resolved = |_: &mut LocalCluster, _: &mut ()| {};
    let on_partition_resolved = |cluster: &mut LocalCluster, _: &mut ()| {
        cluster.check_no_new_roots(400, "PARTITION_TEST", SocketAddrSpace::Unspecified);
    };

    // This kills `max_failures_stake`, so no progress should be made
    run_kill_partition_switch_threshold(
        &[(failures_stake as usize, 16)],
        &[(alive_stake_1 as usize, 8), (alive_stake_2 as usize, 8)],
        None,
        (),
        on_partition_start,
        on_before_partition_resolved,
        on_partition_resolved,
    );
}

#[test]
#[serial]
fn test_kill_partition_switch_threshold_progress() {
    let max_switch_threshold_failure_pct = 1.0 - 2.0 * SWITCH_FORK_THRESHOLD;
    let total_stake = 10_000 * DEFAULT_NODE_STAKE;

    // Kill `< max_failures_stake` of the validators
    let max_failures_stake = (max_switch_threshold_failure_pct * total_stake as f64) as u64;
    let failures_stake = max_failures_stake - 1;
    let total_alive_stake = total_stake - failures_stake;

    // Partition the remaining alive validators, should still make progress
    // once the partition resolves
    let alive_stake_1 = total_alive_stake / 2;
    let alive_stake_2 = total_alive_stake - alive_stake_1;
    let bigger = std::cmp::max(alive_stake_1, alive_stake_2);
    let smaller = std::cmp::min(alive_stake_1, alive_stake_2);

    // At least one of the forks must have > SWITCH_FORK_THRESHOLD in order
    // to guarantee switching proofs can be created. Make sure the other fork
    // is <= SWITCH_FORK_THRESHOLD to make sure progress can be made. Caches
    // bugs such as liveness issues bank-weighted fork choice, which may stall
    // because the fork with less stake could have more weight, but other fork would:
    // 1) Not be able to generate a switching proof
    // 2) Other more staked fork stops voting, so doesn't catch up in bank weight.
    assert!(
        bigger as f64 / total_stake as f64 > SWITCH_FORK_THRESHOLD
            && smaller as f64 / total_stake as f64 <= SWITCH_FORK_THRESHOLD
    );

    let on_partition_start =
        |_: &mut LocalCluster, _: &[Pubkey], _: Vec<ClusterValidatorInfo>, _: &mut ()| {};
    let on_before_partition_resolved = |_: &mut LocalCluster, _: &mut ()| {};
    let on_partition_resolved = |cluster: &mut LocalCluster, _: &mut ()| {
        cluster.check_for_new_roots(16, "PARTITION_TEST", SocketAddrSpace::Unspecified);
    };
    run_kill_partition_switch_threshold(
        &[(failures_stake as usize, 16)],
        &[(alive_stake_1 as usize, 8), (alive_stake_2 as usize, 8)],
        None,
        (),
        on_partition_start,
        on_before_partition_resolved,
        on_partition_resolved,
    );
}

#[test]
#[serial]
#[allow(unused_attributes)]
fn test_duplicate_shreds_switch_failure() {
    solana_logger::setup_with_default(RUST_LOG_FILTER);

    let validator_keypairs = vec![
        "28bN3xyvrP4E8LwEgtLjhnkb7cY4amQb6DrYAbAYjgRV4GAGgkVM2K7wnxnAS7WDneuavza7x21MiafLu1HkwQt4",
        "2saHBBoTkLMmttmPQP8KfBkcCw45S5cwtV3wTdGCscRC8uxdgvHxpHiWXKx4LvJjNJtnNcbSv5NdheokFFqnNDt8",
        "4mx9yoFBeYasDKBGDWCTWGJdWuJCKbgqmuP8bN9umybCh5Jzngw7KQxe99Rf5uzfyzgba1i65rJW4Wqk7Ab5S8ye",
    ]
    .iter()
    .map(|s| (Arc::new(Keypair::from_base58_string(s)), true))
    .collect::<Vec<_>>();
    let validators = validator_keypairs
        .iter()
        .map(|(kp, _)| kp.pubkey())
        .collect::<Vec<_>>();

    // Create 3 nodes:
    // 1) One with > DUPLICATE_THRESHOLD but < 2/3+ supermajority
    // 2) One with < SWITCHING_THRESHOLD so that validator from 1) can't switch to it
    // 3) One bad leader to make duplicate slots
    let total_stake = 100 * DEFAULT_NODE_STAKE;
    let target_switch_fork_stake = (total_stake as f64 * SWITCH_FORK_THRESHOLD) as u64;
    let duplicate_leader_stake = 1;
    let duplicate_fork_node_stake = total_stake - target_switch_fork_stake - duplicate_leader_stake;

    // Ensure the duplicate fork will get duplicate confirmed
    assert!(duplicate_fork_node_stake >= (total_stake as f64 * DUPLICATE_THRESHOLD) as u64);

    let node_stakes = vec![
        duplicate_leader_stake,
        target_switch_fork_stake,
        duplicate_fork_node_stake,
    ];

    let (
        // Has to be first in order to be picked as the duplicate leader
        duplicate_leader_validator_pubkey,
        target_switch_fork_validator_pubkey,
        duplicate_fork_validator_pubkey,
    ) = (validators[0], validators[1], validators[2]);

    info!(
        "duplicate_fork_validator_pubkey: {},
        target_switch_fork_validator_pubkey: {},
        duplicate_leader_validator_pubkey: {}",
        duplicate_fork_validator_pubkey,
        target_switch_fork_validator_pubkey,
        duplicate_leader_validator_pubkey
    );

    let validator_to_slots = vec![
        (duplicate_leader_validator_pubkey, 80),
        (target_switch_fork_validator_pubkey, 80),
        // Give the `duplicate_fork_validator_pubkey` very few leader slots so we have ample time to:
        // 1. Give him a duplicate version of the slot
        // 2. Allow him to vote on that duplicate slot
        // 3. See that the slot was duplicate AFTER he voted
        //
        // Only at this time do we want this validator to have a chance to build a block. Otherwise,
        // if the validator has opportunity to build a block between 2 and 3, they'll build the block
        // on top of the duplicate block, which will possibly includeh is vote for the duplicate block. We
        // want to avoid this because this will make fork choice pick the duplicate block.
        (duplicate_fork_validator_pubkey, 4),
    ];

    let leader_schedule = create_custom_leader_schedule(validator_to_slots.into_iter());

    // 1) Set up the cluster
    let (mut cluster, validator_keypairs) = test_faulty_node(
        BroadcastStageType::BroadcastDuplicates(BroadcastDuplicatesConfig {
            partition: ClusterPartition::Pubkey(vec![duplicate_fork_validator_pubkey]),
        }),
        node_stakes,
        Some(validator_keypairs),
        Some(FixedSchedule {
            leader_schedule: Arc::new(leader_schedule),
        }),
    );

    // 2) Wait for `duplicate_fork_validator_pubkey` to see a duplicate proof
    info!("Waiting for duplicate proof");
    let mut duplicate_proof;
    loop {
        let duplicate_fork_validator_blockstore =
            open_blockstore(&cluster.ledger_path(&duplicate_fork_validator_pubkey));
        duplicate_proof = duplicate_fork_validator_blockstore.get_first_duplicate_proof();
        if duplicate_proof.is_some() {
            break;
        }
        sleep(Duration::from_millis(1000));
    }
    let (dup_slot, duplicate_proof) = duplicate_proof.unwrap();

    // Ensure all the slots <= dup_slot are also full so we know we can replay up to dup_slot
    // on restart
    loop {
        let duplicate_fork_validator_blockstore =
            open_blockstore(&cluster.ledger_path(&duplicate_fork_validator_pubkey));
        if duplicate_fork_validator_blockstore.is_full(dup_slot)
            && duplicate_fork_validator_blockstore.is_connected(dup_slot)
        {
            break;
        }
        sleep(Duration::from_millis(1000));
    }

    // 3) Kill all the validators
    info!("Killing all validators");
    let duplicate_fork_validator_ledger_path =
        cluster.ledger_path(&duplicate_fork_validator_pubkey);
    let mut duplicate_fork_validator_info = cluster.exit_node(&duplicate_fork_validator_pubkey);
    let target_switch_fork_validator_ledger_path =
        cluster.ledger_path(&target_switch_fork_validator_pubkey);
    let target_switch_fork_validator_info = cluster.exit_node(&target_switch_fork_validator_pubkey);
    cluster.exit_node(&duplicate_leader_validator_pubkey);

    let dup_shred1 = Shred::new_from_serialized_shred(duplicate_proof.shred1.clone()).unwrap();
    let dup_shred2 = Shred::new_from_serialized_shred(duplicate_proof.shred2.clone()).unwrap();
    assert_eq!(dup_shred1.slot(), dup_shred2.slot());
    assert_eq!(dup_shred1.slot(), dup_slot);

    info!("Purging towers and ledgers");
    {
        // Purge everything past the `dup_slot` on the `duplicate_fork_validator_pubkey`
        remove_tower(
            &duplicate_fork_validator_ledger_path,
            &duplicate_fork_validator_pubkey,
        );
        let blockstore = open_blockstore(&duplicate_fork_validator_ledger_path);
        // Remove the duplicate proof so that this validator will vote on the `dup_slot`.
        blockstore.remove_slot_duplicate_proof(dup_slot).unwrap();
        purge_slots_with_count(&blockstore, dup_slot + 1, 1000);

        // Purge everything including the `dup_slot` from the `target_switch_fork_validator_pubkey`
        remove_tower(
            &target_switch_fork_validator_ledger_path,
            &target_switch_fork_validator_pubkey,
        );

        let blockstore = open_blockstore(&target_switch_fork_validator_ledger_path);
        purge_slots_with_count(&blockstore, dup_slot, 1000);
    }

    // There should be no duplicate proofs left in blockstore
    {
        let duplicate_fork_validator_blockstore =
            open_blockstore(&duplicate_fork_validator_ledger_path);
        assert!(duplicate_fork_validator_blockstore
            .get_first_duplicate_proof()
            .is_none());
    }

    // Set entrypoint to `target_switch_fork_validator_pubkey` so we can run discovery in gossip even without the
    // bad leader
    cluster.set_entry_point(target_switch_fork_validator_info.info.contact_info.clone());

    // 4) Restart `target_switch_fork_validator_pubkey`, and ensure they vote on their own leader slot
    // not descended from the duplicate slot
    info!("Restarting switch fork node");
    cluster.restart_node(
        &target_switch_fork_validator_pubkey,
        target_switch_fork_validator_info,
        SocketAddrSpace::Unspecified,
    );
    let target_switch_fork_validator_ledger_path =
        cluster.ledger_path(&target_switch_fork_validator_pubkey);

    info!("Waiting for switch fork to make block past duplicate fork");
    loop {
        let last_vote = last_vote_in_tower(
            &target_switch_fork_validator_ledger_path,
            &target_switch_fork_validator_pubkey,
        );
        if let Some((latest_vote_slot, _hash)) = last_vote {
            if latest_vote_slot > dup_slot {
                let blockstore = open_blockstore(&target_switch_fork_validator_ledger_path);
                let ancestor_slots: HashSet<Slot> =
                    AncestorIterator::new_inclusive(latest_vote_slot, &blockstore).collect();
                assert!(ancestor_slots.contains(&latest_vote_slot));
                assert!(ancestor_slots.contains(&0));
                assert!(!ancestor_slots.contains(&dup_slot));
                break;
            }
        }
        sleep(Duration::from_millis(1000));
    }

    // Now restart `duplicate_fork_validator_pubkey`
    // Start the node with partition enabled so he doesn't see the `target_switch_fork_validator_pubkey`
    // before he votes on the duplicate block
    info!("Restarting duplicate fork node");
    let disable_turbine = Arc::new(AtomicBool::new(true));
    duplicate_fork_validator_info.config.turbine_disabled = disable_turbine.clone();
    cluster.restart_node(
        &duplicate_fork_validator_pubkey,
        duplicate_fork_validator_info,
        SocketAddrSpace::Unspecified,
    );
    let duplicate_fork_validator_ledger_path =
        cluster.ledger_path(&duplicate_fork_validator_pubkey);

    // Lift the partition after `duplicate_fork_validator_pubkey` votes on the `dup_slot`
    info!(
        "Waiting on duplicate fork to vote on duplicate slot: {}",
        dup_slot
    );
    loop {
        let last_vote = last_vote_in_tower(
            &duplicate_fork_validator_ledger_path,
            &duplicate_fork_validator_pubkey,
        );
        if let Some((latest_vote_slot, _hash)) = last_vote {
            info!("latest vote: {}", latest_vote_slot);
            if latest_vote_slot == dup_slot {
                break;
            }
        }
        sleep(Duration::from_millis(1000));
    }
    disable_turbine.store(false, Ordering::Relaxed);

    // Send the `duplicate_fork_validator_pubkey` the other version of the
    // shred so they realize it's duplicate
    info!("Resending duplicate shreds to duplicate fork validator");
    let send_socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    let duplicate_fork_validator_tvu = cluster
        .get_contact_info(&duplicate_fork_validator_pubkey)
        .unwrap()
        .tvu;
    send_socket
        .send_to(dup_shred1.payload().as_ref(), &duplicate_fork_validator_tvu)
        .unwrap();
    send_socket
        .send_to(dup_shred2.payload().as_ref(), &duplicate_fork_validator_tvu)
        .unwrap();

    // Check the `duplicate_fork_validator_pubkey` validator to detect a duplicate proof
    info!("Waiting on duplicate fork validator to see duplicate shreds and make a proof",);
    loop {
        let duplicate_fork_validator_blockstore =
            open_blockstore(&duplicate_fork_validator_ledger_path);
        if let Some(dup_proof) = duplicate_fork_validator_blockstore.get_first_duplicate_proof() {
            assert_eq!(dup_proof.0, dup_slot);
            break;
        }
        sleep(Duration::from_millis(1000));
    }

    // Wait for the `duplicate_fork_validator_pubkey` to make another leader block on top
    // of the duplicate fork which includes their own vote for `dup_block`. This
    // should make the duplicate fork the heaviest
    info!("Waiting on duplicate fork validator to generate block on top of duplicate fork",);
    loop {
        let duplicate_fork_validator_blockstore =
            open_blockstore(&cluster.ledger_path(&duplicate_fork_validator_pubkey));
        let meta = duplicate_fork_validator_blockstore
            .meta(dup_slot)
            .unwrap()
            .unwrap();
        if !meta.next_slots.is_empty() {
            info!(
                "duplicate fork validator saw new slots: {:?} on top of duplicate slot",
                meta.next_slots
            );
            break;
        }
        sleep(Duration::from_millis(1000));
    }

    // Check that the cluster is making progress
    cluster.check_for_new_roots(
        16,
        "test_duplicate_shreds_switch_failure",
        SocketAddrSpace::Unspecified,
    );
}

#[test]
#[serial]
#[allow(unused_attributes)]
fn test_duplicate_shreds_broadcast_leader() {
    solana_logger::setup_with_default(RUST_LOG_FILTER);

    // Create 4 nodes:
    // 1) Bad leader sending different versions of shreds to both of the other nodes
    // 2) 1 node who's voting behavior in gossip
    // 3) 1 validator gets the same version as the leader, will see duplicate confirmation
    // 4) 1 validator will not get the same version as the leader. For each of these
    // duplicate slots `S` either:
    //    a) The leader's version of `S` gets > DUPLICATE_THRESHOLD of votes in gossip and so this
    //       node will repair that correct version
    //    b) A descendant `D` of some version of `S` gets > DUPLICATE_THRESHOLD votes in gossip,
    //       but no version of `S` does. Then the node will not know to repair the right version
    //       by just looking at gossip, but will instead have to use EpochSlots repair after
    //       detecting that a descendant does not chain to its version of `S`, and marks that descendant
    //       dead.
    //   Scenarios a) or b) are triggered by our node in 2) who's voting behavior we control.

    // Critical that bad_leader_stake + good_node_stake < DUPLICATE_THRESHOLD and that
    // bad_leader_stake + good_node_stake + our_node_stake > DUPLICATE_THRESHOLD so that
    // our vote is the determining factor
    let bad_leader_stake = 10_000_000 * DEFAULT_NODE_STAKE;
    // Ensure that the good_node_stake is always on the critical path, and the partition node
    // should never be on the critical path. This way, none of the bad shreds sent to the partition
    // node corrupt the good node.
    let good_node_stake = 500 * DEFAULT_NODE_STAKE;
    let our_node_stake = 10_000_000 * DEFAULT_NODE_STAKE;
    let partition_node_stake = DEFAULT_NODE_STAKE;

    let node_stakes = vec![
        bad_leader_stake,
        partition_node_stake,
        good_node_stake,
        // Needs to be last in the vector, so that we can
        // find the id of this node. See call to `test_faulty_node`
        // below for more details.
        our_node_stake,
    ];
    assert_eq!(*node_stakes.last().unwrap(), our_node_stake);
    let total_stake: u64 = node_stakes.iter().sum();

    assert!(
        ((bad_leader_stake + good_node_stake) as f64 / total_stake as f64) < DUPLICATE_THRESHOLD
    );
    assert!(
        (bad_leader_stake + good_node_stake + our_node_stake) as f64 / total_stake as f64
            > DUPLICATE_THRESHOLD
    );

    // Important that the partition node stake is the smallest so that it gets selected
    // for the partition.
    assert!(partition_node_stake < our_node_stake && partition_node_stake < good_node_stake);

    // 1) Set up the cluster
    let (mut cluster, validator_keys) = test_faulty_node(
        BroadcastStageType::BroadcastDuplicates(BroadcastDuplicatesConfig {
            partition: ClusterPartition::Stake(partition_node_stake),
        }),
        node_stakes,
        None,
        None,
    );

    info!("Returned after test_faulty_node");
    // This is why it's important our node was last in `node_stakes`
    let our_id = validator_keys.last().unwrap().pubkey();

    // 2) Kill our node and start up a thread to simulate votes to control our voting behavior
    let our_info = cluster.exit_node(&our_id);
    let node_keypair = our_info.info.keypair;
    let vote_keypair = our_info.info.voting_keypair;
    let bad_leader_id = *cluster.entry_point_info.pubkey();
    let bad_leader_ledger_path = cluster.validators[&bad_leader_id].info.ledger_path.clone();
    info!("our node id: {}", node_keypair.pubkey());

    // 3) Start up a gossip instance to listen for and push votes
    let voter_thread_sleep_ms = 100;
    let gossip_voter = cluster_tests::start_gossip_voter(
        &cluster.entry_point_info.gossip().unwrap(),
        &node_keypair,
        move |(label, leader_vote_tx)| {
            // Filter out votes not from the bad leader
            if label.pubkey() == bad_leader_id {
                let vote = vote_parser::parse_vote_transaction(&leader_vote_tx)
                    .map(|(_, vote, ..)| vote)
                    .unwrap();
                // Filter out empty votes
                if !vote.is_empty() {
                    Some((vote, leader_vote_tx))
                } else {
                    None
                }
            } else {
                None
            }
        },
        {
            let node_keypair = node_keypair.insecure_clone();
            let vote_keypair = vote_keypair.insecure_clone();
            let mut max_vote_slot = 0;
            let mut gossip_vote_index = 0;
            move |latest_vote_slot, leader_vote_tx, parsed_vote, cluster_info| {
                info!("received vote for {}", latest_vote_slot);
                // Add to EpochSlots. Mark all slots frozen between slot..=max_vote_slot.
                if latest_vote_slot > max_vote_slot {
                    let new_epoch_slots: Vec<Slot> =
                        (max_vote_slot + 1..latest_vote_slot + 1).collect();
                    info!(
                        "Simulating epoch slots from our node: {:?}",
                        new_epoch_slots
                    );
                    cluster_info.push_epoch_slots(&new_epoch_slots);
                    max_vote_slot = latest_vote_slot;
                }

                // Only vote on even slots. Note this may violate lockouts if the
                // validator started voting on a different fork before we could exit
                // it above.
                let vote_hash = parsed_vote.hash();
                if latest_vote_slot % 2 == 0 {
                    info!(
                        "Simulating vote from our node on slot {}, hash {}",
                        latest_vote_slot, vote_hash
                    );

                    // Add all recent vote slots on this fork to allow cluster to pass
                    // vote threshold checks in replay. Note this will instantly force a
                    // root by this validator, but we're not concerned with lockout violations
                    // by this validator so it's fine.
                    let leader_blockstore = open_blockstore(&bad_leader_ledger_path);
                    let mut vote_slots: Vec<(Slot, u32)> =
                        AncestorIterator::new_inclusive(latest_vote_slot, &leader_blockstore)
                            .take(MAX_LOCKOUT_HISTORY)
                            .zip(1..)
                            .collect();
                    vote_slots.reverse();
                    let mut vote = VoteStateUpdate::from(vote_slots);
                    let root =
                        AncestorIterator::new_inclusive(latest_vote_slot, &leader_blockstore)
                            .nth(MAX_LOCKOUT_HISTORY);
                    vote.root = root;
                    vote.hash = vote_hash;
                    let vote_tx = vote_transaction::new_compact_vote_state_update_transaction(
                        vote,
                        leader_vote_tx.message.recent_blockhash,
                        &node_keypair,
                        &vote_keypair,
                        &vote_keypair,
                        None,
                    );
                    gossip_vote_index += 1;
                    gossip_vote_index %= MAX_LOCKOUT_HISTORY;
                    cluster_info.push_vote_at_index(vote_tx, gossip_vote_index as u8)
                }
            }
        },
        voter_thread_sleep_ms as u64,
    );

    // 4) Check that the cluster is making progress
    cluster.check_for_new_roots(
        16,
        "test_duplicate_shreds_broadcast_leader",
        SocketAddrSpace::Unspecified,
    );

    // Clean up threads
    gossip_voter.close();
}

#[test]
#[serial]
#[ignore]
fn test_switch_threshold_uses_gossip_votes() {
    solana_logger::setup_with_default(RUST_LOG_FILTER);
    let total_stake = 100 * DEFAULT_NODE_STAKE;

    // Minimum stake needed to generate a switching proof
    let minimum_switch_stake = (SWITCH_FORK_THRESHOLD * total_stake as f64) as u64;

    // Make the heavier stake insufficient for switching so tha the lighter validator
    // cannot switch without seeing a vote from the dead/failure_stake validator.
    let heavier_stake = minimum_switch_stake;
    let lighter_stake = heavier_stake - 1;
    let failures_stake = total_stake - heavier_stake - lighter_stake;

    let partitions: &[(usize, usize)] = &[(heavier_stake as usize, 8), (lighter_stake as usize, 8)];

    #[derive(Default)]
    struct PartitionContext {
        heaviest_validator_key: Pubkey,
        lighter_validator_key: Pubkey,
        dead_validator_info: Option<ClusterValidatorInfo>,
    }

    let on_partition_start = |_cluster: &mut LocalCluster,
                              validator_keys: &[Pubkey],
                              mut dead_validator_infos: Vec<ClusterValidatorInfo>,
                              context: &mut PartitionContext| {
        assert_eq!(dead_validator_infos.len(), 1);
        context.dead_validator_info = Some(dead_validator_infos.pop().unwrap());
        // validator_keys[0] is the validator that will be killed, i.e. the validator with
        // stake == `failures_stake`
        context.heaviest_validator_key = validator_keys[1];
        context.lighter_validator_key = validator_keys[2];
    };

    let on_before_partition_resolved = |_: &mut LocalCluster, _: &mut PartitionContext| {};

    // Check that new roots were set after the partition resolves (gives time
    // for lockouts built during partition to resolve and gives validators an opportunity
    // to try and switch forks)
    let on_partition_resolved = |cluster: &mut LocalCluster, context: &mut PartitionContext| {
        let lighter_validator_ledger_path = cluster.ledger_path(&context.lighter_validator_key);
        let heavier_validator_ledger_path = cluster.ledger_path(&context.heaviest_validator_key);

        let (lighter_validator_latest_vote, _) = last_vote_in_tower(
            &lighter_validator_ledger_path,
            &context.lighter_validator_key,
        )
        .unwrap();

        info!(
            "Lighter validator's latest vote is for slot {}",
            lighter_validator_latest_vote
        );

        // Lighter partition should stop voting after detecting the heavier partition and try
        // to switch. Loop until we see a greater vote by the heavier validator than the last
        // vote made by the lighter validator on the lighter fork.
        let mut heavier_validator_latest_vote;
        let mut heavier_validator_latest_vote_hash;
        let heavier_blockstore = open_blockstore(&heavier_validator_ledger_path);
        loop {
            let (sanity_check_lighter_validator_latest_vote, _) = last_vote_in_tower(
                &lighter_validator_ledger_path,
                &context.lighter_validator_key,
            )
            .unwrap();

            // Lighter validator should stop voting, because `on_partition_resolved` is only
            // called after a propagation time where blocks from the other fork should have
            // finished propagating
            assert_eq!(
                sanity_check_lighter_validator_latest_vote,
                lighter_validator_latest_vote
            );

            let (new_heavier_validator_latest_vote, new_heavier_validator_latest_vote_hash) =
                last_vote_in_tower(
                    &heavier_validator_ledger_path,
                    &context.heaviest_validator_key,
                )
                .unwrap();

            heavier_validator_latest_vote = new_heavier_validator_latest_vote;
            heavier_validator_latest_vote_hash = new_heavier_validator_latest_vote_hash;

            // Latest vote for each validator should be on different forks
            assert_ne!(lighter_validator_latest_vote, heavier_validator_latest_vote);
            if heavier_validator_latest_vote > lighter_validator_latest_vote {
                let heavier_ancestors: HashSet<Slot> =
                    AncestorIterator::new(heavier_validator_latest_vote, &heavier_blockstore)
                        .collect();
                assert!(!heavier_ancestors.contains(&lighter_validator_latest_vote));
                break;
            }
        }

        info!("Checking to make sure lighter validator doesn't switch");
        let mut latest_slot = lighter_validator_latest_vote;

        // Number of chances the validator had to switch votes but didn't
        let mut total_voting_opportunities = 0;
        while total_voting_opportunities <= 5 {
            let (new_latest_slot, latest_slot_ancestors) =
                find_latest_replayed_slot_from_ledger(&lighter_validator_ledger_path, latest_slot);
            latest_slot = new_latest_slot;
            // Ensure `latest_slot` is on the other fork
            if latest_slot_ancestors.contains(&heavier_validator_latest_vote) {
                let tower = restore_tower(
                    &lighter_validator_ledger_path,
                    &context.lighter_validator_key,
                )
                .unwrap();
                // Check that there was an opportunity to vote
                if !tower.is_locked_out(latest_slot, &latest_slot_ancestors) {
                    // Ensure the lighter blockstore has not voted again
                    let new_lighter_validator_latest_vote = tower.last_voted_slot().unwrap();
                    assert_eq!(
                        new_lighter_validator_latest_vote,
                        lighter_validator_latest_vote
                    );
                    info!(
                        "Incrementing voting opportunities: {}",
                        total_voting_opportunities
                    );
                    total_voting_opportunities += 1;
                } else {
                    info!(
                        "Tower still locked out, can't vote for slot: {}",
                        latest_slot
                    );
                }
            } else if latest_slot > heavier_validator_latest_vote {
                warn!(
                    "validator is still generating blocks on its own fork, last processed slot: {}",
                    latest_slot
                );
            }
            sleep(Duration::from_millis(50));
        }

        // Make a vote from the killed validator for slot `heavier_validator_latest_vote` in gossip
        info!(
            "Simulate vote for slot: {} from dead validator",
            heavier_validator_latest_vote
        );
        let vote_keypair = &context
            .dead_validator_info
            .as_ref()
            .unwrap()
            .info
            .voting_keypair
            .clone();
        let node_keypair = &context
            .dead_validator_info
            .as_ref()
            .unwrap()
            .info
            .keypair
            .clone();

        cluster_tests::submit_vote_to_cluster_gossip(
            node_keypair,
            vote_keypair,
            heavier_validator_latest_vote,
            heavier_validator_latest_vote_hash,
            // Make the vote transaction with a random blockhash. Thus, the vote only lives in gossip but
            // never makes it into a block
            Hash::new_unique(),
            cluster
                .get_contact_info(&context.heaviest_validator_key)
                .unwrap()
                .gossip()
                .unwrap(),
            &SocketAddrSpace::Unspecified,
        )
        .unwrap();

        loop {
            // Wait for the lighter validator to switch to the heavier fork
            let (new_lighter_validator_latest_vote, _) = last_vote_in_tower(
                &lighter_validator_ledger_path,
                &context.lighter_validator_key,
            )
            .unwrap();

            if new_lighter_validator_latest_vote != lighter_validator_latest_vote {
                info!(
                    "Lighter validator switched forks at slot: {}",
                    new_lighter_validator_latest_vote
                );
                let (heavier_validator_latest_vote, _) = last_vote_in_tower(
                    &heavier_validator_ledger_path,
                    &context.heaviest_validator_key,
                )
                .unwrap();
                let (smaller, larger) =
                    if new_lighter_validator_latest_vote > heavier_validator_latest_vote {
                        (
                            heavier_validator_latest_vote,
                            new_lighter_validator_latest_vote,
                        )
                    } else {
                        (
                            new_lighter_validator_latest_vote,
                            heavier_validator_latest_vote,
                        )
                    };

                // Check the new vote is on the same fork as the heaviest fork
                let heavier_blockstore = open_blockstore(&heavier_validator_ledger_path);
                let larger_slot_ancestors: HashSet<Slot> =
                    AncestorIterator::new(larger, &heavier_blockstore)
                        .chain(std::iter::once(larger))
                        .collect();
                assert!(larger_slot_ancestors.contains(&smaller));
                break;
            } else {
                sleep(Duration::from_millis(50));
            }
        }
    };

    let ticks_per_slot = 8;
    run_kill_partition_switch_threshold(
        &[(failures_stake as usize, 0)],
        partitions,
        Some(ticks_per_slot),
        PartitionContext::default(),
        on_partition_start,
        on_before_partition_resolved,
        on_partition_resolved,
    );
}

#[test]
#[serial]
fn test_listener_startup() {
    let mut config = ClusterConfig {
        node_stakes: vec![DEFAULT_NODE_STAKE],
        cluster_lamports: DEFAULT_CLUSTER_LAMPORTS,
        num_listeners: 3,
        validator_configs: make_identical_validator_configs(
            &ValidatorConfig::default_for_test(),
            1,
        ),
        ..ClusterConfig::default()
    };
    let cluster = LocalCluster::new(&mut config, SocketAddrSpace::Unspecified);
    let cluster_nodes = discover_cluster(
        &cluster.entry_point_info.gossip().unwrap(),
        4,
        SocketAddrSpace::Unspecified,
    )
    .unwrap();
    assert_eq!(cluster_nodes.len(), 4);
}

fn find_latest_replayed_slot_from_ledger(
    ledger_path: &Path,
    mut latest_slot: Slot,
) -> (Slot, HashSet<Slot>) {
    loop {
        let mut blockstore = open_blockstore(ledger_path);
        // This is kind of a hack because we can't query for new frozen blocks over RPC
        // since the validator is not voting.
        let new_latest_slots: Vec<Slot> = blockstore
            .slot_meta_iterator(latest_slot)
            .unwrap()
            .filter_map(|(s, _)| if s > latest_slot { Some(s) } else { None })
            .collect();

        for new_latest_slot in new_latest_slots {
            latest_slot = new_latest_slot;
            info!("Checking latest_slot {}", latest_slot);
            // Wait for the slot to be fully received by the validator
            loop {
                info!("Waiting for slot {} to be full", latest_slot);
                if blockstore.is_full(latest_slot) {
                    break;
                } else {
                    sleep(Duration::from_millis(50));
                    blockstore = open_blockstore(ledger_path);
                }
            }
            // Wait for the slot to be replayed
            loop {
                info!("Waiting for slot {} to be replayed", latest_slot);
                if blockstore.get_bank_hash(latest_slot).is_some() {
                    return (
                        latest_slot,
                        AncestorIterator::new(latest_slot, &blockstore).collect(),
                    );
                } else {
                    sleep(Duration::from_millis(50));
                    blockstore = open_blockstore(ledger_path);
                }
            }
        }
        sleep(Duration::from_millis(50));
    }
}

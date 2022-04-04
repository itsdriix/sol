//! DoS tool
//!
//! Sends requests to cluster in a loop to measure
//! the effect of handling these requests on the performance of the cluster.
//!
//! * `mode` argument defines interface to use (e.g. rpc, tvu, tpu)
//! * `data-type` argument specifies the type of the request.
//! Some request types might be used only with particular `mode` value.
//! For example, `get-account-info` is valid only with `mode=rpc`.
//!
//! Most options are provided for `data-type = transaction`.
//! These options allow to compose transaction which fails at
//! a particular stage of the processing pipeline.
//!
//! Example 1: send random transactions to TPU
//! ```bash
//! solana-dos --entrypoint 127.0.0.1:8001 --mode tpu --data-type random
//! ```
//!
//! Example 2: send unique transactions with valid recent blockhash to TPU
//! ```bash
//! solana-dos --entrypoint 127.0.0.1:8001 --mode tpu --data-type random
//! solana-dos --entrypoint 127.0.0.1:8001 --mode tpu \
//!     --data-type transaction --generate-unique-transactions
//!     --payer config/bootstrap-validator/identity.json \
//!     --generate-valid-blockhash
//! ```
//!
#![allow(clippy::integer_arithmetic)]

use {
    itertools::Itertools,
    log::*,
    rand::{thread_rng, Rng},
    solana_client::rpc_client::RpcClient,
    solana_core::serve_repair::RepairProtocol,
    solana_dos::cli::*,
    solana_gossip::{contact_info::ContactInfo, gossip_service::discover},
    solana_sdk::{
        hash::Hash,
        instruction::{AccountMeta, CompiledInstruction, Instruction},
        pubkey::Pubkey,
        signature::{read_keypair_file, Keypair, Signature, Signer},
        stake,
        system_instruction::SystemInstruction,
        system_program,
        transaction::Transaction,
    },
    solana_streamer::socket::SocketAddrSpace,
    std::{
        net::{SocketAddr, UdpSocket},
        process::exit,
        str::FromStr,
        time::{Duration, Instant},
    },
};

static REPORT_EACH_MILLIS: u128 = 10_000;
fn compute_tps(count: usize) -> usize {
    (count * 1000) / (REPORT_EACH_MILLIS as usize)
}

fn get_repair_contact(nodes: &[ContactInfo]) -> ContactInfo {
    let source = thread_rng().gen_range(0, nodes.len());
    let mut contact = nodes[source].clone();
    contact.id = solana_sdk::pubkey::new_rand();
    contact
}

struct TransactionGenerator {
    blockhash: Hash,
    last_generated: Instant,
    transaction_params: TransactionParams,
}

impl TransactionGenerator {
    fn new(transaction_params: TransactionParams) -> Self {
        TransactionGenerator {
            blockhash: Hash::default(),
            last_generated: (Instant::now() - Duration::from_secs(100)),
            transaction_params,
        }
    }

    fn generate(
        &mut self,
        payer: Option<&Keypair>,
        kpvals: Option<Vec<&Keypair>>, // provided for valid signatures
        rpc_client: &Option<RpcClient>,
    ) -> Transaction {
        // generate a new blockhash every 1sec
        if self.transaction_params.valid_blockhash
            && self.last_generated.elapsed().as_millis() > 1000
        {
            self.blockhash = rpc_client.as_ref().unwrap().get_latest_blockhash().unwrap();
            self.last_generated = Instant::now();
        }

        // in order to evaluate the performance implications of the different transactions
        // we create here transactions which are filtered out on different stages of processing pipeline

        // create an arbitrary valid instruction
        let lamports = 5;
        let transfer_instruction = SystemInstruction::Transfer { lamports };
        let program_ids = vec![system_program::id(), stake::program::id()];

        // transaction with payer, in this case signatures are valid and num_signatures is irrelevant
        // random payer will cause error "attempt to debit an account but found no record of a prior credit"
        // if payer is correct, it will trigger error with not enough signatures
        if let Some(payer) = payer {
            let instruction = Instruction::new_with_bincode(
                program_ids[0],
                &transfer_instruction,
                vec![
                    AccountMeta::new(program_ids[0], false),
                    AccountMeta::new(program_ids[1], false),
                ],
            );
            Transaction::new_signed_with_payer(
                &[instruction],
                Some(&payer.pubkey()),
                &[payer],
                self.blockhash,
            )
        } else if self.transaction_params.valid_signatures {
            // Since we don't provide a payer, this transaction will end up
            // filtered at legacy.rs sanitize method (banking_stage) with error "a program cannot be payer"

            let instructions = vec![CompiledInstruction::new(
                0,
                &transfer_instruction,
                vec![0, 1],
            )];

            let keypairs = kpvals.unwrap();
            Transaction::new_with_compiled_instructions(
                &keypairs,
                &[],
                self.blockhash,
                program_ids,
                instructions,
            )
        } else {
            // Since we provided invalid signatures
            // this transaction will end up filtered at legacy.rs (banking_stage) because
            // num_required_signatures == 0
            let instructions = vec![CompiledInstruction::new(
                0,
                &transfer_instruction,
                vec![0, 1],
            )];

            let mut tx = Transaction::new_with_compiled_instructions(
                &[] as &[&Keypair; 0],
                &[],
                self.blockhash,
                program_ids,
                instructions,
            );
            tx.signatures = vec![Signature::new_unique(); self.transaction_params.num_signatures];
            tx
        }
    }
}

fn get_target_and_client(
    nodes: &[ContactInfo],
    mode: Mode,
    entrypoint_addr: SocketAddr,
) -> (Option<SocketAddr>, Option<RpcClient>) {
    let mut target = None;
    let mut rpc_client = None;
    if nodes.is_empty() {
        if mode == Mode::Rpc {
            rpc_client = Some(RpcClient::new_socket(entrypoint_addr));
        }
        target = Some(entrypoint_addr);
    } else {
        info!("************ NODE ***********");
        for node in nodes {
            info!("{:?}", node);
        }
        info!("ADDR = {}", entrypoint_addr);

        for node in nodes {
            if node.gossip == entrypoint_addr {
                info!("{}", node.gossip);
                target = match mode {
                    Mode::Gossip => Some(node.gossip),
                    Mode::Tvu => Some(node.tvu),
                    Mode::TvuForwards => Some(node.tvu_forwards),
                    Mode::Tpu => {
                        rpc_client = Some(RpcClient::new_socket(node.rpc));
                        Some(node.tpu)
                    }
                    Mode::TpuForwards => Some(node.tpu_forwards),
                    Mode::Repair => Some(node.repair),
                    Mode::ServeRepair => Some(node.serve_repair),
                    Mode::Rpc => {
                        rpc_client = Some(RpcClient::new_socket(node.rpc));
                        None
                    }
                };
                break;
            }
        }
    }
    (target, rpc_client)
}

fn run_dos_rpc_mode(
    rpc_client: Option<RpcClient>,
    iterations: usize,
    data_type: DataType,
    data_input: Option<String>,
) {
    let mut last_log = Instant::now();
    let mut total_count: usize = 0;
    let mut count = 0;
    let mut error_count = 0;
    loop {
        match data_type {
            DataType::GetAccountInfo => {
                let res = rpc_client
                    .as_ref()
                    .unwrap()
                    .get_account(&Pubkey::from_str(data_input.as_ref().unwrap()).unwrap());
                if res.is_err() {
                    error_count += 1;
                }
            }
            DataType::GetProgramAccounts => {
                let res = rpc_client
                    .as_ref()
                    .unwrap()
                    .get_program_accounts(&Pubkey::from_str(data_input.as_ref().unwrap()).unwrap());
                if res.is_err() {
                    error_count += 1;
                }
            }
            _ => {
                panic!("unsupported data type");
            }
        }
        count += 1;
        total_count += 1;
        if last_log.elapsed().as_millis() > REPORT_EACH_MILLIS {
            info!(
                "count: {}, errors: {}, tps: {}",
                count,
                error_count,
                compute_tps(count)
            );
            last_log = Instant::now();
            count = 0;
        }
        if iterations != 0 && total_count >= iterations {
            break;
        }
    }
}

fn apply_permutation<'a, T>(indexes: Vec<&usize>, items: &'a [T]) -> Vec<&'a T> {
    let mut res = Vec::with_capacity(indexes.len());
    for i in indexes {
        res.push(&items[*i]);
    }
    res
}

fn run_dos_transactions(
    rpc_client: Option<RpcClient>,
    target: SocketAddr,
    iterations: usize,
    payer: Option<&Keypair>,
    transaction_params: TransactionParams,
) {
    info!("{:?}", transaction_params);
    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    let num_signatures = transaction_params.num_signatures;
    let valid_signatures = transaction_params.valid_signatures;
    let mut transaction_generator = TransactionGenerator::new(transaction_params);

    let mut last_log = Instant::now();
    let mut total_count: usize = 0;
    let mut count: usize = 0;
    let mut error_count = 0;
    let mut generation_elapsed: u64 = 0;
    let mut send_elapsed: u64 = 0;

    let start = Instant::now();

    // Generate n=1000 unique keypairs, which are used to create
    // chunks of keypairs.
    // The number of chunck is described by binomial coefficient
    // and hence 1000 seems to be a reasonable choice
    let mut keypairs_flat: Vec<Keypair> = Vec::new();
    if valid_signatures {
        keypairs_flat = (0..1000 * num_signatures).map(|_| Keypair::new()).collect();
    }
    let indexes: Vec<usize> = (0..keypairs_flat.len()).collect();
    let mut it = indexes.iter().permutations(num_signatures);
    info!(
        "Keypairs generation took {} micros",
        start.elapsed().as_micros()
    );

    loop {
        let generation_start = Instant::now();
        let chunk_keypairs = if valid_signatures {
            let permut = it.next();
            if permut.is_none() {
                keypairs_flat.iter_mut().for_each(|v| *v = Keypair::new());
                info!("Regenerate keypairs");
                continue;
            }
            let permut = permut.unwrap();
            Some(apply_permutation(permut, keypairs_flat.as_slice()))
        } else {
            None
        };

        let tx = transaction_generator.generate(payer, chunk_keypairs, &rpc_client);
        generation_elapsed =
            generation_elapsed.saturating_add(generation_start.elapsed().as_micros() as u64);

        let send_start = Instant::now();
        let data = bincode::serialize(&tx).unwrap();
        let res = socket.send_to(&data, target);
        send_elapsed = send_elapsed.saturating_add(send_start.elapsed().as_micros() as u64);

        if res.is_err() {
            error_count += 1;
        }
        count += 1;
        total_count += 1;
        if last_log.elapsed().as_millis() > REPORT_EACH_MILLIS {
            info!("count: {}, errors: {}", count, error_count);
            info!(
                "Generation avg time (micros): {}, sending avg time(micros): {}, tps: {}",
                generation_elapsed as f64 / (count as f64),
                send_elapsed as f64 / (count as f64),
                compute_tps(count),
            );
            last_log = Instant::now();
            count = 0;

            generation_elapsed = 0;
            send_elapsed = 0;
        }
        if iterations != 0 && total_count >= iterations {
            break;
        }
    }
}

fn run_dos(
    nodes: &[ContactInfo],
    iterations: usize,
    payer: Option<&Keypair>,
    params: DosClientParameters,
) {
    let (target, rpc_client) = get_target_and_client(nodes, params.mode, params.entrypoint_addr);
    let target = target.expect("should have target");
    info!("Targeting {}", target);

    if params.mode == Mode::Rpc {
        run_dos_rpc_mode(rpc_client, iterations, params.data_type, params.data_input);
    } else if params.data_type == DataType::Transaction
        && params.transaction_params.unique_transactions
    {
        run_dos_transactions(
            rpc_client,
            target,
            iterations,
            payer,
            params.transaction_params,
        );
    } else {
        let mut data = match params.data_type {
            DataType::RepairHighest => {
                let slot = 100;
                let req =
                    RepairProtocol::WindowIndexWithNonce(get_repair_contact(nodes), slot, 0, 0);
                bincode::serialize(&req).unwrap()
            }
            DataType::RepairShred => {
                let slot = 100;
                let req = RepairProtocol::HighestWindowIndexWithNonce(
                    get_repair_contact(nodes),
                    slot,
                    0,
                    0,
                );
                bincode::serialize(&req).unwrap()
            }
            DataType::RepairOrphan => {
                let slot = 100;
                let req = RepairProtocol::OrphanWithNonce(get_repair_contact(nodes), slot, 0);
                bincode::serialize(&req).unwrap()
            }
            DataType::Random => {
                vec![0; params.data_size]
            }
            DataType::Transaction => {
                let tp = params.transaction_params;
                info!("{:?}", tp);

                let mut transaction_generator = TransactionGenerator::new(tp);
                let tx = transaction_generator.generate(payer, None, &rpc_client);
                info!("{:?}", tx);
                bincode::serialize(&tx).unwrap()
            }
            _ => panic!("Unsupported data_type detected"),
        };

        let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
        let mut last_log = Instant::now();
        let mut total_count: usize = 0;
        let mut count: usize = 0;
        let mut error_count = 0;
        loop {
            if params.data_type == DataType::Random {
                thread_rng().fill(&mut data[..]);
            }
            let res = socket.send_to(&data, target);
            if res.is_err() {
                error_count += 1;
            }
            count += 1;
            total_count += 1;
            if last_log.elapsed().as_millis() > REPORT_EACH_MILLIS {
                info!(
                    "count: {}, errors: {}, tps: {}",
                    count,
                    error_count,
                    compute_tps(count)
                );
                last_log = Instant::now();
                count = 0;
            }
            if iterations != 0 && total_count >= iterations {
                break;
            }
        }
    }
}

fn main() {
    solana_logger::setup_with_default("solana=info");
    let cmd_params = build_cli_parameters();

    let mut nodes = vec![];
    if !cmd_params.skip_gossip {
        info!("Finding cluster entry: {:?}", cmd_params.entrypoint_addr);
        let socket_addr_space = SocketAddrSpace::new(cmd_params.allow_private_addr);
        let (gossip_nodes, _validators) = discover(
            None, // keypair
            Some(&cmd_params.entrypoint_addr),
            None,                              // num_nodes
            Duration::from_secs(60),           // timeout
            None,                              // find_node_by_pubkey
            Some(&cmd_params.entrypoint_addr), // find_node_by_gossip_addr
            None,                              // my_gossip_addr
            0,                                 // my_shred_version
            socket_addr_space,
        )
        .unwrap_or_else(|err| {
            eprintln!(
                "Failed to discover {} node: {:?}",
                cmd_params.entrypoint_addr, err
            );
            exit(1);
        });
        nodes = gossip_nodes;
    }

    info!("done found {} nodes", nodes.len());
    let payer = cmd_params
        .transaction_params
        .payer_filename
        .as_ref()
        .map(|keypair_file_name| {
            read_keypair_file(&keypair_file_name)
                .unwrap_or_else(|_| panic!("bad keypair {:?}", keypair_file_name))
        });

    run_dos(&nodes, 0, payer.as_ref(), cmd_params);
}

#[cfg(test)]
pub mod test {
    use {
        super::*,
        solana_local_cluster::{cluster::Cluster, local_cluster::LocalCluster},
        solana_sdk::timing::timestamp,
    };

    #[test]
    fn test_dos() {
        let nodes = [ContactInfo::new_localhost(
            &solana_sdk::pubkey::new_rand(),
            timestamp(),
        )];
        let entrypoint_addr = nodes[0].gossip;

        run_dos(
            &nodes,
            1,
            None,
            DosClientParameters {
                entrypoint_addr,
                mode: Mode::Tvu,
                data_size: 10,
                data_type: DataType::Random,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams::default(),
            },
        );

        run_dos(
            &nodes,
            1,
            None,
            DosClientParameters {
                entrypoint_addr,
                mode: Mode::Repair,
                data_size: 10,
                data_type: DataType::RepairHighest,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams::default(),
            },
        );

        run_dos(
            &nodes,
            1,
            None,
            DosClientParameters {
                entrypoint_addr,
                mode: Mode::ServeRepair,
                data_size: 10,
                data_type: DataType::RepairShred,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams::default(),
            },
        );
    }

    #[test]
    #[ignore]
    fn test_dos_local_cluster_transactions() {
        let num_nodes = 1;
        let cluster =
            LocalCluster::new_with_equal_stakes(num_nodes, 100, 3, SocketAddrSpace::Unspecified);
        assert_eq!(cluster.validators.len(), num_nodes);

        let nodes = cluster.get_node_pubkeys();
        let node = cluster.get_contact_info(&nodes[0]).unwrap().clone();
        let nodes_slice = [node];

        // send random transactions to TPU
        // will be discarded on sigverify stage
        run_dos(
            &nodes_slice,
            1,
            None,
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                mode: Mode::Tpu,
                data_size: 1024,
                data_type: DataType::Random,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams::default(),
            },
        );

        // send transactions to TPU with 2 random signatures
        // will be filtered on dedup (because transactions are not unique)
        run_dos(
            &nodes_slice,
            1,
            None,
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                mode: Mode::Tpu,
                data_size: 0, // irrelevant if not random
                data_type: DataType::Transaction,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams {
                    num_signatures: 2,
                    valid_blockhash: false,
                    valid_signatures: false,
                    unique_transactions: false,
                    payer_filename: None,
                },
            },
        );

        // send *unique* transactions to TPU with 4 random signatures
        // will be discarded on banking stage in legacy.rs
        // ("there should be at least 1 RW fee-payer account")
        run_dos(
            &nodes_slice,
            1,
            None,
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                mode: Mode::Tpu,
                data_size: 0, // irrelevant if not random
                data_type: DataType::Transaction,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams {
                    num_signatures: 4,
                    valid_blockhash: false,
                    valid_signatures: false,
                    unique_transactions: true,
                    payer_filename: None,
                },
            },
        );

        // send unique transactions to TPU with 2 random signatures
        // will be discarded on banking stage in legacy.rs (A program cannot be a payer)
        // because we haven't provided a valid payer
        run_dos(
            &nodes_slice,
            1,
            None,
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                mode: Mode::Tpu,
                data_size: 0, // irrelevant if not random
                data_type: DataType::Transaction,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams {
                    num_signatures: 2,
                    valid_blockhash: false, // irrelevant without valid payer, because
                    // it will be filtered before blockhash validity checks
                    valid_signatures: true,
                    unique_transactions: true,
                    payer_filename: None,
                },
            },
        );

        // send unique transaction to TPU with valid blockhash
        // will be discarded due to invalid hash
        run_dos(
            &nodes_slice,
            1,
            Some(&cluster.funding_keypair),
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                mode: Mode::Tpu,
                data_size: 0, // irrelevant if not random
                data_type: DataType::Transaction,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams {
                    num_signatures: 2,
                    valid_blockhash: false,
                    valid_signatures: true,
                    unique_transactions: true,
                    payer_filename: None,
                },
            },
        );

        // send unique transaction to TPU with valid blockhash
        // will fail with error processing Instruction 0: missing required signature for instruction
        run_dos(
            &nodes_slice,
            1,
            Some(&cluster.funding_keypair),
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                mode: Mode::Tpu,
                data_size: 0, // irrelevant if not random
                data_type: DataType::Transaction,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams {
                    num_signatures: 2,
                    valid_blockhash: true,
                    valid_signatures: true,
                    unique_transactions: true,
                    payer_filename: None,
                },
            },
        );
    }

    #[test]
    #[ignore]
    fn test_dos_local_cluster() {
        solana_logger::setup();
        let num_nodes = 1;
        let cluster =
            LocalCluster::new_with_equal_stakes(num_nodes, 100, 3, SocketAddrSpace::Unspecified);
        assert_eq!(cluster.validators.len(), num_nodes);

        let nodes = cluster.get_node_pubkeys();
        let node = cluster.get_contact_info(&nodes[0]).unwrap().clone();

        run_dos(
            &[node],
            10_000_000,
            Some(&cluster.funding_keypair),
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                mode: Mode::Tpu,
                data_size: 0, // irrelevant if not random
                data_type: DataType::Transaction,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams {
                    num_signatures: 2,
                    valid_blockhash: true,
                    valid_signatures: true,
                    unique_transactions: true,
                    payer_filename: None,
                },
            },
        );
    }

    #[test]
    #[ignore]
    fn test_dos_random() {
        solana_logger::setup();
        let num_nodes = 1;
        let cluster =
            LocalCluster::new_with_equal_stakes(num_nodes, 100, 3, SocketAddrSpace::Unspecified);
        assert_eq!(cluster.validators.len(), num_nodes);

        let nodes = cluster.get_node_pubkeys();
        let node = cluster.get_contact_info(&nodes[0]).unwrap().clone();
        let nodes_slice = [node];

        // send random transactions to TPU
        // will be discarded on sigverify stage
        run_dos(
            &nodes_slice,
            100000,
            None,
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                mode: Mode::Tpu,
                data_size: 1024,
                data_type: DataType::Random,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams::default(),
            },
        );
    }
    #[test]
    #[ignore]
    fn test_dos_unique() {
        solana_logger::setup();
        let num_nodes = 1;
        let cluster =
            LocalCluster::new_with_equal_stakes(num_nodes, 100, 3, SocketAddrSpace::Unspecified);
        assert_eq!(cluster.validators.len(), num_nodes);

        let nodes = cluster.get_node_pubkeys();
        let node = cluster.get_contact_info(&nodes[0]).unwrap().clone();
        let nodes_slice = [node];

        // send unique transaction to TPU with valid blockhash
        // will fail with error processing Instruction 0: missing required signature for instruction
        run_dos(
            &nodes_slice,
            100000,
            None,
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                mode: Mode::Tpu,
                data_size: 0, // irrelevant if not random
                data_type: DataType::Transaction,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams {
                    num_signatures: 4,
                    valid_blockhash: true,
                    valid_signatures: true,
                    unique_transactions: true,
                    payer_filename: None,
                },
            },
        );
    }
}

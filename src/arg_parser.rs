use crate::args::{Args, BalancesArgs, Command, DistributeArgs};
use clap::{value_t, value_t_or_exit, App, Arg, ArgMatches, SubCommand};
use solana_clap_utils::input_validators::is_valid_signer;
use solana_cli_config::CONFIG_FILE;
use std::ffi::OsString;
use std::process::exit;

fn get_matches<'a, I, T>(args: I) -> ArgMatches<'a>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let default_config_file = CONFIG_FILE.as_ref().unwrap();
    App::new("solana-stake-accounts")
        .about("about")
        .version("version")
        .arg(
            Arg::with_name("config_file")
                .long("config")
                .takes_value(true)
                .value_name("FILEPATH")
                .default_value(default_config_file)
                .help("Config file"),
        )
        .arg(
            Arg::with_name("url")
                .long("url")
                .global(true)
                .takes_value(true)
                .value_name("URL")
                .help("RPC entrypoint address. i.e. http://devnet.solana.com"),
        )
        .subcommand(
            SubCommand::with_name("distribute")
                .about("Distribute tokens")
                .arg(
                    Arg::with_name("transactions_csv")
                        .required(true)
                        .index(1)
                        .takes_value(true)
                        .value_name("FILE")
                        .help("Transactions CSV file"),
                )
                .arg(
                    Arg::with_name("bids_csv")
                        .long("bids-csv")
                        .required(true)
                        .takes_value(true)
                        .value_name("FILE")
                        .help("Bids CSV file"),
                )
                .arg(
                    Arg::with_name("dollars_per_sol")
                        .long("dollars-per-sol")
                        .required(true)
                        .takes_value(true)
                        .value_name("NUMBER")
                        .help("Dollars per SOL"),
                )
                .arg(
                    Arg::with_name("dry_run")
                        .long("dry-run")
                        .help("Do not execute any transfers"),
                )
                .arg(
                    Arg::with_name("sender_keypair")
                        .long("from")
                        .takes_value(true)
                        .value_name("SENDING_KEYPAIR")
                        .validator(is_valid_signer)
                        .help("Keypair to fund accounts"),
                )
                .arg(
                    Arg::with_name("fee_payer")
                        .long("fee-payer")
                        .takes_value(true)
                        .value_name("KEYPAIR")
                        .validator(is_valid_signer)
                        .help("Fee payer"),
                ),
        )
        .subcommand(
            SubCommand::with_name("balances")
                .about("Balance of each account")
                .arg(
                    Arg::with_name("bids_csv")
                        .long("bids-csv")
                        .required(true)
                        .takes_value(true)
                        .value_name("FILE")
                        .help("Bids CSV file"),
                )
                .arg(
                    Arg::with_name("dollars_per_sol")
                        .long("dollars-per-sol")
                        .required(true)
                        .takes_value(true)
                        .value_name("NUMBER")
                        .help("Dollars per SOL"),
                ),
        )
        .get_matches_from(args)
}

fn parse_distribute_args(matches: &ArgMatches<'_>) -> DistributeArgs<String> {
    DistributeArgs {
        bids_csv: value_t_or_exit!(matches, "bids_csv", String),
        transactions_csv: value_t_or_exit!(matches, "transactions_csv", String),
        dollars_per_sol: value_t_or_exit!(matches, "dollars_per_sol", f64),
        dry_run: matches.is_present("dry_run"),
        sender_keypair: value_t!(matches, "sender_keypair", String).ok(),
        fee_payer: value_t!(matches, "fee_payer", String).ok(),
    }
}

fn parse_balances_args(matches: &ArgMatches<'_>) -> BalancesArgs {
    BalancesArgs {
        bids_csv: value_t_or_exit!(matches, "bids_csv", String),
        dollars_per_sol: value_t_or_exit!(matches, "dollars_per_sol", f64),
    }
}

pub fn parse_args<I, T>(args: I) -> Args<String>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = get_matches(args);
    let config_file = matches.value_of("config_file").unwrap().to_string();
    let url = matches.value_of("url").map(|x| x.to_string());

    let command = match matches.subcommand() {
        ("distribute", Some(matches)) => Command::Distribute(parse_distribute_args(matches)),
        ("balances", Some(matches)) => Command::Balances(parse_balances_args(matches)),
        _ => {
            eprintln!("{}", matches.usage());
            exit(1);
        }
    };
    Args {
        config_file,
        url,
        command,
    }
}

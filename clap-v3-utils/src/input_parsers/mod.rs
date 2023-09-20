use {
    crate::input_validators::normalize_to_url_if_moniker,
    chrono::DateTime,
    clap::ArgMatches,
    solana_sdk::{
        clock::UnixTimestamp, commitment_config::CommitmentConfig, genesis_config::ClusterType,
        native_token::sol_to_lamports, pubkey::MAX_SEED_LEN,
    },
    std::str::FromStr,
};

mod signer;
pub use signer::*;

// Return parsed values from matches at `name`
pub fn values_of<T>(matches: &ArgMatches, name: &str) -> Option<Vec<T>>
where
    T: std::str::FromStr,
    <T as std::str::FromStr>::Err: std::fmt::Debug,
{
    matches
        .values_of(name)
        .map(|xs| xs.map(|x| x.parse::<T>().unwrap()).collect())
}

// Return a parsed value from matches at `name`
pub fn value_of<T>(matches: &ArgMatches, name: &str) -> Option<T>
where
    T: std::str::FromStr,
    <T as std::str::FromStr>::Err: std::fmt::Debug,
{
    matches.value_of(name)
        .map(|value| value.parse::<T>().ok())
}

pub fn unix_timestamp_from_rfc3339_datetime(
    matches: &ArgMatches,
    name: &str,
) -> Option<UnixTimestamp> {
    matches.value_of(name).and_then(|value| {
        DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|date_time| date_time.timestamp())
    })
}

#[deprecated(
    since = "1.17.0",
    note = "please use `UiTokenAmount::parse_amount` and `UiTokenAmount::sol_to_lamport` instead"
)]
pub fn lamports_of_sol(matches: &ArgMatches, name: &str) -> Option<u64> {
    value_of(matches, name).map(sol_to_lamports)
}

pub fn cluster_type_of(matches: &ArgMatches, name: &str) -> Option<ClusterType> {
    value_of(matches, name)
}

pub fn commitment_of(matches: &ArgMatches, name: &str) -> Option<CommitmentConfig> {
    matches
        .value_of(name)
        .map(|value| CommitmentConfig::from_str(value).unwrap_or_default())
}

pub fn parse_url(arg: &str) -> Result<String, String> {
    url::Url::parse(arg)
        .map_err(|err| err.to_string())
        .and_then(|url| {
            url.has_host()
                .then_some(|| arg.to_string())
                .ok_or("no host provided".to_string())
        })
}

pub fn parse_url_or_moniker(arg: &str) -> Result<String, String> {
    match url::Url::parse(&normalize_to_url_if_moniker(arg)) {
        Ok(url) => {
            if url.has_host() {
                Ok(arg.to_string())
            } else {
                Err("no host provided".to_string())
            }
        }
        Err(err) => Err(format!("{err}")),
    }
}

pub fn parse_pow2(arg: &str) -> Result<usize, String> {
    arg.parse::<usize>()
        .map_err(|e| format!("Unable to parse, provided: {arg}, err: {e}"))
        .and_then(|v| {
            v.is_power_of_two()
                .then_some(v)
                .ok_or(format!("Must be a power of 2: {v}"))
        })
}

pub fn parse_percentage(arg: &str) -> Result<u8, String> {
    arg.parse::<u8>()
        .map_err(|e| format!("Unable to parse input percentage, provided: {arg}, err: {e}"))
        .and_then(|v| {
            (v <= 100)
                .then_some(v)
                .ok_or(format!(
                    "Percentage must be in range of 0 to 100, provided: {v}"
                ))
        })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UiTokenAmount {
    Amount(f64),
    All,
}
impl UiTokenAmount {
    pub fn parse_amount(arg: &str) -> Result<UiTokenAmount, String> {
        arg.parse::<f64>()
            .map(UiTokenAmount::Amount)
            .map_err(|_| {
                format!("Unable to parse input amount, provided: {arg}")
            })
    }

    pub fn parse_amount_or_all(arg: &str) -> Result<UiTokenAmount, String> {
        if arg == "ALL" {
            Ok(UiTokenAmount::All)
        } else {
            parse_amount(arg).map_err(|_| {
                format!(
                    "Unable to parse input amount as float or 'ALL' keyword, provided: {arg}"
            })
        }
    }

    pub fn to_raw_amount(&self, decimals: u8) -> RawTokenAmount {
        match self {
            UiTokenAmount::Amount(amount) => {
                RawTokenAmount::Amount((amount * 10_usize.pow(decimals as u32) as f64) as u64)
            }
            UiTokenAmount::All => RawTokenAmount::All,
        }
    }

    pub fn sol_to_lamport(&self) -> RawTokenAmount {
        const NATIVE_SOL_DECIMALS: u8 = 9;
        self.to_raw_amount(NATIVE_SOL_DECIMALS)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RawTokenAmount {
    Amount(u64),
    All,
}

pub fn parse_rfc3339_datetime(arg: &str) -> Result<String, String> {
    DateTime::parse_from_rfc3339(arg)
        .map(|_| arg.to_string())
        .map_err(|e| format!("{e}"))
}

pub fn parse_derivation(arg: &str) -> Result<String, String> {
    let value = arg.replace('\'', "");
    let mut parts = value.split('/');
    let account = parts.next().unwrap();
    account
        .parse::<u32>()
        .map_err(|e| format!("Unable to parse derivation, provided: {account}, err: {e}"))
        .and_then(|_| {
            if let Some(change) = parts.next() {
                change.parse::<u32>().map_err(|e| {
                    format!("Unable to parse derivation, provided: {change}, err: {e}")
                })
            } else {
                Ok(0)
            }
        })?;
    Ok(arg.to_string())
}

pub fn parse_structured_seed(arg: &str) -> Result<String, String> {
    let (prefix, value) = arg
        .split_once(':')
        .ok_or("Seed must contain ':' as delimiter")
        .unwrap();
    if prefix.is_empty() || value.is_empty() {
        Err(String::from("Seed prefix or value is empty"))
    } else {
        match prefix {
            "string" | "pubkey" | "hex" | "u8" => Ok(arg.to_string()),
            _ => {
                let len = prefix.len();
                if len != 5 && len != 6 {
                    Err(format!("Wrong prefix length {len} {prefix}:{value}"))
                } else {
                    let sign = &prefix[0..1];
                    let type_size = &prefix[1..len.saturating_sub(2)];
                    let byte_order = &prefix[len.saturating_sub(2)..len];
                    if sign != "u" && sign != "i" {
                        Err(format!("Wrong prefix sign {sign} {prefix}:{value}"))
                    } else if type_size != "16"
                        && type_size != "32"
                        && type_size != "64"
                        && type_size != "128"
                    {
                        Err(format!(
                            "Wrong prefix type size {type_size} {prefix}:{value}"
                        ))
                    } else if byte_order != "le" && byte_order != "be" {
                        Err(format!(
                            "Wrong prefix byte order {byte_order} {prefix}:{value}"
                        ))
                    } else {
                        Ok(arg.to_string())
                    }
                }
            }
        }
    }
}

pub fn parse_derived_address_seed(arg: &str) -> Result<String, String> {
    (arg.len() <= MAX_SEED_LEN)
        .then_some(arg.to_string())
        .ok_or(|| format!(
            "Address seed must not be longer than {MAX_SEED_LEN} bytes"
        ))
}
#[cfg(test)]
mod tests {
    use {
        super::*,
        clap::{Arg, Command},
        solana_sdk::{hash::Hash, pubkey::Pubkey},
    };

    fn app<'ab>() -> Command<'ab> {
        Command::new("test")
            .arg(
                Arg::new("multiple")
                    .long("multiple")
                    .takes_value(true)
                    .multiple_occurrences(true)
                    .multiple_values(true),
            )
            .arg(Arg::new("single").takes_value(true).long("single"))
            .arg(Arg::new("unit").takes_value(true).long("unit"))
    }

    #[test]
    fn test_values_of() {
        let matches = app().get_matches_from(vec!["test", "--multiple", "50", "--multiple", "39"]);
        assert_eq!(values_of(&matches, "multiple"), Some(vec![50, 39]));
        assert_eq!(values_of::<u64>(&matches, "single"), None);

        let pubkey0 = solana_sdk::pubkey::new_rand();
        let pubkey1 = solana_sdk::pubkey::new_rand();
        let matches = app().get_matches_from(vec![
            "test",
            "--multiple",
            &pubkey0.to_string(),
            "--multiple",
            &pubkey1.to_string(),
        ]);
        assert_eq!(
            values_of(&matches, "multiple"),
            Some(vec![pubkey0, pubkey1])
        );
    }

    #[test]
    fn test_value_of() {
        let matches = app().get_matches_from(vec!["test", "--single", "50"]);
        assert_eq!(value_of(&matches, "single"), Some(50));
        assert_eq!(value_of::<u64>(&matches, "multiple"), None);

        let pubkey = solana_sdk::pubkey::new_rand();
        let matches = app().get_matches_from(vec!["test", "--single", &pubkey.to_string()]);
        assert_eq!(value_of(&matches, "single"), Some(pubkey));
    }

    #[test]
    fn test_parse_pubkey() {
        let command = Command::new("test").arg(
            Arg::new("pubkey")
                .long("pubkey")
                .takes_value(true)
                .value_parser(clap::value_parser!(Pubkey)),
        );

        // success case
        let matches = command
            .clone()
            .try_get_matches_from(vec!["test", "--pubkey", "11111111111111111111111111111111"])
            .unwrap();
        assert_eq!(
            *matches.get_one::<Pubkey>("pubkey").unwrap(),
            Pubkey::from_str("11111111111111111111111111111111").unwrap(),
        );

        // validation fails
        let matches_error = command
            .clone()
            .try_get_matches_from(vec!["test", "--pubkey", "this_is_an_invalid_arg"])
            .unwrap_err();
        assert_eq!(matches_error.kind, clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn test_parse_hash() {
        let command = Command::new("test").arg(
            Arg::new("hash")
                .long("hash")
                .takes_value(true)
                .value_parser(clap::value_parser!(Hash)),
        );

        // success case
        let matches = command
            .clone()
            .try_get_matches_from(vec!["test", "--hash", "11111111111111111111111111111111"])
            .unwrap();
        assert_eq!(
            *matches.get_one::<Hash>("hash").unwrap(),
            Hash::from_str("11111111111111111111111111111111").unwrap(),
        );

        // validation fails
        let matches_error = command
            .clone()
            .try_get_matches_from(vec!["test", "--hash", "this_is_an_invalid_arg"])
            .unwrap_err();
        assert_eq!(matches_error.kind, clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn test_parse_token_amount() {
        let command = Command::new("test").arg(
            Arg::new("amount")
                .long("amount")
                .takes_value(true)
                .value_parser(UiTokenAmount::parse_amount),
        );

        // success cases
        let matches = command
            .clone()
            .try_get_matches_from(vec!["test", "--amount", "11223344"])
            .unwrap();
        assert_eq!(
            *matches.get_one::<UiTokenAmount>("amount").unwrap(),
            UiTokenAmount::Amount(11223344_f64),
        );

        let matches = command
            .clone()
            .try_get_matches_from(vec!["test", "--amount", "0.11223344"])
            .unwrap();
        assert_eq!(
            *matches.get_one::<UiTokenAmount>("amount").unwrap(),
            UiTokenAmount::Amount(0.11223344),
        );

        // validation fail cases
        let matches_error = command
            .clone()
            .try_get_matches_from(vec!["test", "--amount", "this_is_an_invalid_arg"])
            .unwrap_err();
        assert_eq!(matches_error.kind, clap::error::ErrorKind::ValueValidation);

        let matches_error = command
            .clone()
            .try_get_matches_from(vec!["test", "--amount", "all"])
            .unwrap_err();
        assert_eq!(matches_error.kind, clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn test_parse_token_amount_or_all() {
        let command = Command::new("test").arg(
            Arg::new("amount")
                .long("amount")
                .takes_value(true)
                .value_parser(UiTokenAmount::parse_amount_or_all),
        );

        // success cases
        let matches = command
            .clone()
            .try_get_matches_from(vec!["test", "--amount", "11223344"])
            .unwrap();
        assert_eq!(
            *matches.get_one::<UiTokenAmount>("amount").unwrap(),
            UiTokenAmount::Amount(11223344_f64),
        );

        let matches = command
            .clone()
            .try_get_matches_from(vec!["test", "--amount", "0.11223344"])
            .unwrap();
        assert_eq!(
            *matches.get_one::<UiTokenAmount>("amount").unwrap(),
            UiTokenAmount::Amount(0.11223344),
        );

        let matches = command
            .clone()
            .try_get_matches_from(vec!["test", "--amount", "ALL"])
            .unwrap();
        assert_eq!(
            *matches.get_one::<UiTokenAmount>("amount").unwrap(),
            UiTokenAmount::All,
        );

        // validation fail cases
        let matches_error = command
            .clone()
            .try_get_matches_from(vec!["test", "--amount", "this_is_an_invalid_arg"])
            .unwrap_err();
        assert_eq!(matches_error.kind, clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn test_sol_to_lamports() {
        let command = Command::new("test").arg(
            Arg::new("amount")
                .long("amount")
                .takes_value(true)
                .value_parser(UiTokenAmount::parse_amount_or_all),
        );

        let test_cases = vec![
            ("50", 50_000_000_000),
            ("1.5", 1_500_000_000),
            ("0.03", 30_000_000),
        ];

        for (arg, expected_lamport) in test_cases {
            let matches = command
                .clone()
                .try_get_matches_from(vec!["test", "--amount", arg])
                .unwrap();
            assert_eq!(
                matches
                    .get_one::<UiTokenAmount>("amount")
                    .unwrap()
                    .sol_to_lamport(),
                RawTokenAmount::Amount(expected_lamport),
            );
        }
    }

    #[test]
    fn test_derivation() {
        let command = Command::new("test").arg(
            Arg::new("derivation")
                .long("derivation")
                .takes_value(true)
                .value_parser(parse_derivation),
        );

        let test_arguments = vec![
            ("2", true),
            ("0", true),
            ("65537", true),
            ("0/2", true),
            ("a", false),
            ("4294967296", false),
            ("a/b", false),
            ("0/4294967296", false),
        ];

        for (arg, should_accept) in test_arguments {
            if should_accept {
                let matches = command
                    .clone()
                    .try_get_matches_from(vec!["test", "--derivation", arg])
                    .unwrap();
                assert_eq!(matches.get_one::<String>("derivation").unwrap(), arg);
            }
        }
    }
}

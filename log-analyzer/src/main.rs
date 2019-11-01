extern crate byte_unit;

use bincode;
use byte_unit::Byte;
use clap::{
    crate_description, crate_name, crate_version, value_t_or_exit, App, Arg, ArgMatches, SubCommand,
};
use serde::{Deserialize, Serialize};
use serde_json::{self};
use std::collections::HashMap;
use std::fs;
use std::ops::Sub;
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
struct LogLine {
    a: String,
    b: String,
    a_to_b: String,
    b_to_a: String,
}

impl Default for LogLine {
    fn default() -> Self {
        Self {
            a: String::default(),
            b: String::default(),
            a_to_b: "0B".to_string(),
            b_to_a: "0B".to_string(),
        }
    }
}

impl Sub for &LogLine {
    type Output = String;

    fn sub(self, rhs: Self) -> Self::Output {
        let a_to_b = Byte::from_str(&self.a_to_b)
            .expect("Failed to read a_to_b bytes")
            .get_bytes();
        let b_to_a = Byte::from_str(&self.b_to_a)
            .expect("Failed to read b_to_a bytes")
            .get_bytes();
        let rhs_a_to_b = Byte::from_str(&rhs.a_to_b)
            .expect("Failed to read a_to_b bytes")
            .get_bytes();
        let rhs_b_to_a = Byte::from_str(&rhs.b_to_a)
            .expect("Failed to read b_to_a bytes")
            .get_bytes();
        let out1 = if a_to_b > rhs_b_to_a {
            format!(
                "Lost {} bytes from {} to {}",
                a_to_b - rhs_b_to_a,
                self.a,
                self.b
            )
        } else if a_to_b < rhs_b_to_a {
            format!(
                "Lost {} bytes from {} to {}",
                rhs_b_to_a - a_to_b,
                self.b,
                self.a
            )
        } else {
            String::default()
        };
        let out2 = if rhs_a_to_b > b_to_a {
            format!(
                "Lost {} bytes from {} to {}",
                rhs_a_to_b - b_to_a,
                self.a,
                self.b
            )
        } else if a_to_b < rhs_b_to_a {
            format!(
                "Lost {} bytes from {} to {}",
                b_to_a - rhs_a_to_b,
                self.b,
                self.a
            )
        } else {
            String::default()
        };
        out1 + &out2
    }
}

fn process_iftop_logs(matches: &ArgMatches) {
    let map_address;
    let private_address;
    let public_address;
    match matches.subcommand() {
        ("map-IP", Some(args_matches)) => {
            map_address = true;
            if let Some(addr) = args_matches.value_of("priv") {
                private_address = addr;
            } else {
                panic!("Private IP address must be provided");
            };
            if let Some(addr) = args_matches.value_of("pub") {
                public_address = addr;
            } else {
                panic!("Private IP address must be provided");
            };
        }
        _ => {
            map_address = false;
            private_address = "";
            public_address = "";
        }
    };

    let log_path = PathBuf::from(value_t_or_exit!(matches, "file", String));
    let mut log = fs::read_to_string(&log_path).expect("Unable to read log file");
    log.insert(0, '[');
    let terminate_at = log.rfind('}').expect("Didn't find a terminating '}'") + 1;
    let _ = log.split_off(terminate_at);
    log.push(']');
    let json_log: Vec<LogLine> = serde_json::from_str(&log).expect("Failed to parse log as JSON");

    let mut unique_latest_logs = HashMap::new();

    json_log.into_iter().rev().for_each(|l| {
        if !l.a.is_empty() && !l.b.is_empty() && !l.a_to_b.is_empty() && !l.b_to_a.is_empty() {
            let key = (l.a.clone(), l.b.clone());
            unique_latest_logs.entry(key).or_insert(l);
        }
    });
    let output: Vec<LogLine> = unique_latest_logs
        .into_iter()
        .map(|(_, l)| {
            if map_address {
                LogLine {
                    a: l.a.replace(private_address, public_address),
                    b: l.b.replace(private_address, public_address),
                    a_to_b: l.a_to_b,
                    b_to_a: l.b_to_a,
                }
            } else {
                l
            }
        })
        .collect();

    println!("{:?}", bincode::serialize(&output).unwrap());
}

fn compare_logs(matches: &ArgMatches) {
    let dir_path = PathBuf::from(value_t_or_exit!(matches, "dir", String));
    if !dir_path.is_dir() {
        panic!("Need a folder that contains all log files");
    }
    let files = fs::read_dir(dir_path).expect("Failed to read log folder");
    let logs: Vec<_> = files
        .into_iter()
        .flat_map(|f| {
            if let Ok(f) = f {
                let log_str = fs::read_to_string(&f.path()).expect("Unable to read log file");
                let log: Vec<LogLine> =
                    bincode::deserialize(log_str.as_bytes()).expect("Failed to deserialize log");
                log
            } else {
                vec![]
            }
        })
        .collect();
    let mut logs_hash = HashMap::new();
    logs.iter().for_each(|l| {
        let key = (l.a.clone(), l.b.clone());
        logs_hash.entry(key).or_insert(l);
    });

    logs.iter().for_each(|l| {
        let v1 = logs_hash.remove(&(l.a.clone(), l.b.clone()));
        let v2 = logs_hash.remove(&(l.b.clone(), l.a.clone()));
        if let Some(v1) = v1 {
            if let Some(v2) = v2 {
                println!("{}", v1 - v2);
            } else {
                println!("{}", v1 - &LogLine::default());
            }
        } else if let Some(v2) = v2 {
            println!("{}", v2 - &LogLine::default());
        }
    });
}

fn main() {
    solana_logger::setup();

    let matches = App::new(crate_name!())
        .about(crate_description!())
        .version(crate_version!())
        .subcommand(
            SubCommand::with_name("iftop")
                .about("Process iftop log file")
                .arg(
                    Arg::with_name("file")
                        .short("f")
                        .long("file")
                        .value_name("iftop log file")
                        .takes_value(true)
                        .help("Location of the log file generated by iftop"),
                )
                .subcommand(
                    SubCommand::with_name("map-IP")
                        .about("Public IP Address")
                        .arg(
                            Arg::with_name("priv")
                                .long("priv")
                                .value_name("IP Address")
                                .takes_value(true)
                                .required(true)
                                .help("The private IP address that should be mapped"),
                        )
                        .arg(
                            Arg::with_name("pub")
                                .long("pub")
                                .value_name("IP Address")
                                .takes_value(true)
                                .required(true)
                                .help("The public IP address"),
                        ),
                ),
        )
        .subcommand(
            SubCommand::with_name("compare")
                .about("Compare processed iftop log files")
                .arg(
                    Arg::with_name("dir")
                        .short("d")
                        .long("dir")
                        .value_name("DIR")
                        .takes_value(true)
                        .help("Location of processed log files"),
                ),
        )
        .get_matches();

    match matches.subcommand() {
        ("iftop", Some(args_matches)) => process_iftop_logs(args_matches),
        ("compare", Some(args_matches)) => compare_logs(args_matches),
        _ => {}
    };
}

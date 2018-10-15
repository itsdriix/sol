extern crate bincode;
extern crate byteorder;
extern crate elf;
extern crate libc;
extern crate rbpf;
extern crate solana_program_interface;
#[macro_use]
extern crate serde_derive;

pub mod bpf_verifier;

use bincode::{deserialize, serialize};
use byteorder::{ByteOrder, LittleEndian, WriteBytesExt};
use solana_program_interface::account::KeyedAccount;
use solana_program_interface::loader_instruction::LoaderInstruction;
use solana_program_interface::pubkey::Pubkey;
use std::env;
use std::io::prelude::*;
use std::mem;
use std::path::PathBuf;
use std::str;

/// Dynamic link library prefixs
const PLATFORM_FILE_PREFIX_BPF: &str = "";

/// Dynamic link library file extension specific to the platform
const PLATFORM_FILE_EXTENSION_BPF: &str = "o";

/// Section name
pub const PLATFORM_SECTION_RS: &str = ".text,entrypoint";
pub const PLATFORM_SECTION_C: &str = ".text.entrypoint";

fn create_path(name: &str) -> PathBuf {
        let pathbuf = {
            let current_exe = env::current_exe().unwrap();
            PathBuf::from(current_exe.parent().unwrap().parent().unwrap())
        };

        pathbuf.join(PathBuf::from(PLATFORM_FILE_PREFIX_BPF.to_string() + name)
                .with_extension(PLATFORM_FILE_EXTENSION_BPF)
        )
    }

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum ProgramError {
    // TODO
    Overflow,
    UserdataDeserializeFailure,
}

#[allow(dead_code)]
fn dump_prog(name: &str, prog: &[u8]) {
    let mut eight_bytes: Vec<u8> = Vec::new();
    println!("BPF Program: {}", name);
    for i in prog.iter() {
        if eight_bytes.len() >= 7 {
            println!("{:02X?}", eight_bytes);
            eight_bytes.clear();
        } else {
            eight_bytes.push(i.clone());
        }
    }
}

fn serialize_state(infos: &mut [KeyedAccount], data: &[u8]) -> Vec<u8> {
    assert_eq!(32, mem::size_of::<Pubkey>());

    let mut v: Vec<u8> = Vec::new();
    v.write_u64::<LittleEndian>(infos.len() as u64).unwrap();
    for info in infos.iter_mut() {
        v.write_all(info.key.as_ref()).unwrap();
        v.write_i64::<LittleEndian>(info.account.tokens).unwrap();
        v.write_u64::<LittleEndian>(info.account.userdata.len() as u64)
            .unwrap();
        v.write_all(&info.account.userdata).unwrap();
        v.write_all(info.account.program_id.as_ref()).unwrap();
    }
    v.write_u64::<LittleEndian>(data.len() as u64).unwrap();
    v.write_all(data).unwrap();
    v
}

fn deserialize_state(infos: &mut [KeyedAccount], buffer: &[u8]) {
    assert_eq!(32, mem::size_of::<Pubkey>());

    let mut start = mem::size_of::<u64>();
    for info in infos.iter_mut() {
        start += mem::size_of::<Pubkey>(); // skip pubkey
        info.account.tokens = LittleEndian::read_i64(&buffer[start..]);

        start += mem::size_of::<u64>() // skip tokens
                  + mem::size_of::<u64>(); // skip length tag
        let end = start + info.account.userdata.len();
        info.account.userdata.clone_from_slice(&buffer[start..end]);

        start += info.account.userdata.len() // skip userdata
                  + mem::size_of::<Pubkey>(); // skip program_id
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum BpfLoader {
    File { name: String },
    Bits { bits: Vec<u8> },
}

#[no_mangle]
pub extern "C" fn process(keyed_accounts: &mut [KeyedAccount], tx_data: &[u8]) -> bool {
    println!("sobpf: AccountInfos: {:#?}", keyed_accounts);
    println!("sobpf: data: {:?} len: {}", tx_data, tx_data.len());

    if keyed_accounts[0].account.executable {
        // TODO do this stuff in a cleaner way
        let prog: Vec<u8>;
        if let Ok(program) = deserialize(&keyed_accounts[0].account.userdata) {
            match program {
                BpfLoader::File { name } => {
                    println!("Call Bpf with file {:?}", name);
                    let path = create_path(&name);
                    let file = match elf::File::open_path(&path) {
                        Ok(f) => f,
                        Err(e) => panic!("Error opening ELF {:?}: {:?}", path, e),
                    };

                    let text_section = match file.get_section(PLATFORM_SECTION_RS) {
                        Some(s) => s,
                        None => match file.get_section(PLATFORM_SECTION_C) {
                            Some(s) => s,
                            None => panic!("Failed to find text section"),
                        },
                    };
                    prog = text_section.data.clone();
                }
                BpfLoader::Bits { bits } => {
                    println!("Call Bpf with bits");
                    prog = bits;
                }
            }
        } else {
            println!("deserialize failed: {:?}", tx_data);
            return false;
        }
        println!("Call BPF, {} Instructions", prog.len() / 8);

        let mut vm = rbpf::EbpfVmRaw::new(&prog, Some(bpf_verifier::verifier));

        // TODO register more handlers (e.g: signals, memcpy, etc...)
        vm.register_helper(
            rbpf::helpers::BPF_TRACE_PRINTK_IDX,
            rbpf::helpers::bpf_trace_printf,
        );

        let mut v = serialize_state(&mut keyed_accounts[1..], &tx_data);
        if 0 == vm.prog_exec(v.as_mut_slice()) {
            println!("BPF program failed");
            return false;
        }
        deserialize_state(&mut keyed_accounts[1..], &v);
    } else if let Ok(instruction) = deserialize(tx_data) {
            println!("BpfLoader process_transaction: {:?}", instruction);
            match instruction {
                LoaderInstruction::Write { offset, bits } => {
                    println!("LoaderInstruction::Write offset {} bits {:?}", offset, bits);
                    let offset = offset as usize;
                    if keyed_accounts[0].account.userdata.len() <= offset + bits.len() {
                        return false;
                    }
                    // TODO support both name and bits?  only name supported now
                    // TODO this should be in finalize
                    let name = match str::from_utf8(&bits) {
                        Ok(s) => s.to_string(),
                        Err(e) => panic!("Invalid UTF-8 sequence: {}", e),
                    };
                    println!("name: {:?}", name);
                    let s = serialize(&BpfLoader::File { name }).unwrap();
                    keyed_accounts[0]
                        .account
                        .userdata
                        .splice(0..s.len(), s.iter().cloned());
                }

                LoaderInstruction::Finalize => {
                    keyed_accounts[0].account.executable = true;
                    // TODO move this to spawn
                    keyed_accounts[0].account.loader_program_id =
                        keyed_accounts[0].account.program_id;
                    keyed_accounts[0].account.program_id = *keyed_accounts[0].key;
                    println!(
                        "BpfLoader Finalize prog: {:?} loader {:?}",
                        keyed_accounts[0].account.program_id,
                        keyed_accounts[0].account.loader_program_id
                    );
                }
            }
        } else {
            println!("Invalid program transaction: {:?}", tx_data);
            return false;
        }
    true
}

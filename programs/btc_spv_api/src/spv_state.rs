use crate::utils::*;
use crate::header_store::*;
use crate::header_store::HeaderStoreError;
use std::{error, fmt};
use serde_derive::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};
use log::*;


pub type BitcoinTxHash = [u8;32];

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct BlockHeader {
    // Bitcoin network version
    pub version     : u32,
    // Previous block's hash/digest
    pub parent      : BitcoinTxHash,
    // merkle Root of the block, proofEntry side should be None
    pub merkle_root : ProofEntry,
    // the blocktime associate with the block
    pub time        : u32,
    // An encoded version of the target threshold this block’s header hash must be less than or equal to.
    pub nbits       : u32,

    pub nonce       : u32,
    // Block hash
    pub blockhash   : BitcoinTxHash,
}

impl BlockHeader {
    pub fn new(header: &[u8;80], blockhash: &[u8;32]) -> Result<BlockHeader, SpvError> {

        let version    = header[0 .. 4]; // version is largely useless because of miners messing with the last 2 bytes
        // extract digest from last block
        let parentHash = header[4 .. 36];
        // extract merkle root in internal byte order
        let merkleRoot = header[36 .. 68];
        // timestamp associate with the block
        let blockTime  = u32::from_le_bytes(header[68 .. 72]);
        // nbits field is an encoded version of the
        let nbits      = header[72 .. 76];

        let nonce      = header[76 .. 80];

        BlockHeader {
            version: version,
            parent: parentHash,
            merkle_root: merkleRoot,
            time: blockTime,
            nbits: nbits,
            nonce: nonce,
            blockhash: blockhash,
        }
    }

    pub fn hexnew(header: &str, blockhash: &str) -> Result<BlockHeader, SpvError> {
        if header.len() != 160 || blockhash.len() != 64 {
            Err(SpvError::InvalidBlockHeader)
        }
        let bhbytes = decode_hex(blockhash)?;

        match decode_hex(header){
            Ok(header) => {
                Ok(BlockHeader::new(&header, &bhbytes))
            }
            Err(e) => {
                Err(e)
            }
        }
    }

    pub fn difficulty(mut self) -> u32 {
        // calculates difficulty from nbits

    }
}



pub type HeaderChain = Vec<BlockHeader>;
// a vector of BlockHeaders used as part of a Proof
// index 0    : the block header of the block prior to the proof Block
// index 1    : the block header of the proof block
// index 2-n* : the block headers for the confirmation chain
// (where n is the confirmations value from the proof request)

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct ProofEntry {
    // 32 byte merkle hashes
    pub hash: [u8;32],
    // side of the merkle tree entry
    pub side: EntrySide,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub enum EntrySide {
    // Left side of the hash combination
    Left,
    // Right side of hash combination
    Right,
    // Root hash (neither side)
    Root,
}

pub type MerkleProof = Vec<ProofEntry>;
// a vector of ProofEntries used as part of a Proof
// index 0     : a ProofEntry representing the txid
// indices 0-n : ProofEntries linking the txhash and the merkle root
// index n     : a ProofEntry representing the merkel root for the block in question


#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct ClientRequestInfo {
    // bitcoin transaction hash
    pub txHash:        BitcoinTxHash,
    // confirmation count
    pub confirmations: u8,
    // fee paid for tx verification
    pub fee:           u64,
    // required minimum difficulty for submitted blocks
    pub difficulty:    u64,
    // expiration slot height
    pub expiration:    Option<u32>,
}



#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct ProofRequest {
    pub owner:         Pubkey,
    // bitcoin transaction hash
    pub txHash:        BitcoinTxHash,
    // confirmation count
    pub confirmations: u8,
    // fee paid for tx verification
    pub fee:           u64,
    // minimum allowable difficulty
    pub difficulty:    u64,
    // expiration slot height
    pub expiration:    u64,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct Proof {
    // the pubkey who submitted the proof in question, entitled to fees from any corresponding proof requests
    pub submitter:  Pubkey,
    // merkle branch connecting txhash to block header merkle root
    pub proof:      MerkleProof,
    // chain of bitcoin headers provifing context for the proof
    pub headers:    HeaderChain,
    // txhash associated with the Proof
    pub transaction:Transaction,
    // public key of the request this proof corresponds to
    pub request: Pubkey,
}


#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub enum AccountState {
    // Request Account
    Request(ClientRequestInfo),
    // Verified Proof
    Verification(Proof),
    // Account's userdata is Unallocated
    Unallocated,
    // Invalid
    Invalid,
}

impl Default for AccountState {
    fn default() -> Self {
        AccountState::Unallocated
    }
}

///Errors
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub enum SpvError {
    InvalidBlockHeader,

    HeaderStoreError,

    ParseError,
}

impl error::Error for SpvError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
    // temporary measure
    None
    }
}

impl From<HeaderStoreError> for SpvError {
    fn from(e: HeaderStoreError) -> Self {
        SpvError::HeaderStoreError
    }
}

impl From<DecodeHexError> for SpvError {
    fn from(e: DecodeHexError) -> Self {
        SpvError::ParseError
    }
}

impl fmt::Debug for SpvError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result{
        match self {
            SpvError::InvalidBlockHeader  => "BlockHeader is malformed or does not apply ".fmt(f),
            SpvError::HeaderStoreError => "Placeholder headerstore error debug text".fmt(f),
            SpvError::ParseError => "Error parsing blockheaders debug".fmt(f),
        }
    }
}

impl fmt::Display for SpvError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            SpvError::InvalidBlockHeader  => "BlockHeader is malformed or does not apply ".fmt(f),
            SpvError::HeaderStoreError => "Placeholder headerstore error text".fmt(f),
            SpvError::ParseError => "Error parsing blockheaders placceholder text".fmt(f),
        }
    }
}

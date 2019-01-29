use crate::entry;
use crate::packet::BLOB_HEADER_SIZE;

use solana_sdk::hash::Hash;
use solana_sdk::signature::{Keypair, KeypairUtil};

use std::fs;
use std::path::PathBuf;

use super::*;

fn get_tmp_ledger_path(name: &str) -> Result<PathBuf> {
    use std::env;
    let out_dir = env::var("OUT_DIR").unwrap_or_else(|_| "target".to_string());
    let keypair = Keypair::new();

    let path: PathBuf = [
        out_dir,
        "tmp".into(),
        format!("store-{}-{}", name, keypair.pubkey()),
    ]
    .iter()
    .collect();

    // whack any possible collision
    let _ignore = fs::remove_dir_all(&path);
    fs::create_dir_all(&path)?;

    Ok(path)
}

#[test]
fn test_get_put_simple() {
    let p = get_tmp_ledger_path("test_get_put_simple").unwrap();
    let store = Store::open(&p);
    let slot = 0;

    // simple metadata insert
    let meta = SlotMeta::new(slot, 1);
    store
        .put_meta(0, meta.clone())
        .expect("couldn't insert slotmeta");
    let meta2 = store.get_meta(0).expect("couldn't retrieve slotmeta");

    assert_eq!(meta, meta2);

    // simple blob insert
    let entries = entry::make_tiny_test_entries(1);
    let blob = entries[0].to_blob();

    store.put_blob(&blob).expect("couldn't insert blob");
    let (slot, idx) = (blob.slot().unwrap(), blob.index().unwrap());
    let out_blob = store.get_blob(slot, idx).expect("couldn't retrieve blob");

    assert_eq!(blob, out_blob);

    // simple erasure insert
    let code: Vec<u8> = (0u8..255u8).cycle().take(1024).collect();
    store
        .put_erasure(5, 2, &code)
        .expect("couldn't insert erasure");
    let out_code = store.get_erasure(5, 2).expect("couldn't retrieve erasure");

    assert_eq!(code, out_code);

    drop(store);
    Store::destroy(&p).expect("destruction should succeed");
}

#[test]
fn test_insert_noncontiguous_blobs() {
    let p = get_tmp_ledger_path("test_insert_noncontiguous_blobs").unwrap();
    let store = Store::open(&p);

    // try inserting some blobs
    let entries = entry::make_tiny_test_entries(10);

    let e2_iter = entries.iter().enumerate().map(|(idx, entry)| {
        let mut b = entry.to_blob();
        b.set_slot(0).unwrap();
        b.set_index(idx as u64 + 20).unwrap();
        b
    });

    let blobs: Vec<_> = entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let mut b = entry.to_blob();
            b.set_slot(0).unwrap();
            b.set_index(idx as u64).unwrap();
            b
        })
        .chain(e2_iter)
        .collect();

    store
        .insert_blobs(&blobs)
        .expect("unable to insert entries");

    let blob_bytes = blobs
        .into_iter()
        .map(|blob| {
            let ser_data = &blob.data[..BLOB_HEADER_SIZE + blob.size().unwrap()];
            Vec::from(ser_data)
        })
        .collect::<Vec<Vec<u8>>>();

    let retrieved: Result<Vec<_>> = store
        .slot_data_from(0, 0..)
        .expect("couldn't create slot daaa iterator")
        .collect();
    let retrieved = retrieved.expect("Bad iterator somehow or something");

    assert_eq!(blob_bytes.len(), retrieved.len());
    for (input, retrieved) in blob_bytes.iter().zip(retrieved.iter()) {
        assert_eq!(input.len(), retrieved.len());
        assert_eq!(input, retrieved);
    }

    let meta = store.get_meta(0).unwrap();
    assert_eq!(meta.received, 29);
    assert_eq!(meta.consumed, 9);

    drop(store);
    Store::destroy(&p).expect("destruction should succeed");
}

#[test]
fn test_ensure_correct_metadata() {
    let p = get_tmp_ledger_path("get-put-simple").unwrap();
    let store = Store::open(&p);
    let num_ticks = store.config.ticks_per_block * store.config.num_blocks_per_slot;
    let slot = 1;

    // try inserting ticks to fill a slot
    let entries = entry::create_ticks(num_ticks as usize, Hash::default());

    // Skip slot 0 because bootstrap slot has a different expected amount of ticks
    let blobs: Vec<_> = entries
        .into_iter()
        .enumerate()
        .map(|(idx, mut entry)| {
            entry.tick_height = idx as u64;
            let mut b = entry.to_blob();
            b.set_slot(slot).unwrap();
            b.set_index(idx as u64).unwrap();
            b
        })
        .collect();

    store
        .insert_blobs(&blobs)
        .expect("unable to insert entries");

    let meta = store.get_meta(slot).unwrap();
    println!(
        "meta = {:?}, expected_ticks = {}",
        meta,
        meta.num_expected_ticks(&store.config)
    );

    assert_eq!(meta.received, num_ticks - 1);
    assert_eq!(meta.consumed, num_ticks - 1);
    assert_eq!(meta.consumed_ticks, num_ticks - 1);
    assert!(meta.contains_all_ticks(&store.config));

    drop(store);
    Store::destroy(&p).expect("destruction should succeed");
}

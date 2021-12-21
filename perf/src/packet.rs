//! The `packet` module defines data structures and methods to pull data from the network.
pub use solana_sdk::packet::{ExtendedPacket, Meta, Packet, PacketInterface, PACKET_DATA_SIZE};
use {
    crate::{cuda_runtime::PinnedVec, recycler::Recycler},
    bincode::config::Options,
    serde::Serialize,
    std::net::SocketAddr,
};

pub const NUM_PACKETS: usize = 1024 * 8;

pub const PACKETS_PER_BATCH: usize = 128;
pub const NUM_RCVMMSGS: usize = 128;

#[derive(Debug, Default, Clone)]
pub struct PacketBatch<P: PacketInterface> {
    pub packets: PinnedVec<P>,
}

// TODO: Rename `StandardPackets` to `StandardPacketBatch`, also potentially examine
//       if we want to call this something else besides "Standard"
pub type StandardPackets = PacketBatch<Packet>;
pub type ExtendedPacketBatch = PacketBatch<ExtendedPacket>;

pub type PacketBatchRecycler<P> = Recycler<PinnedVec<P>>;
pub type StandardPacketBatchRecycler = PacketBatchRecycler<Packet>;

impl<P: PacketInterface> PacketBatch<P> {
    pub fn new(packets: Vec<P>) -> Self {
        let packets = PinnedVec::from_vec(packets);
        Self { packets }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let packets = PinnedVec::with_capacity(capacity);
        Self { packets }
    }

    pub fn new_unpinned_with_recycler(
        recycler: PacketBatchRecycler<P>,
        size: usize,
        name: &'static str,
    ) -> Self {
        let mut packets = recycler.allocate(name);
        packets.reserve(size);
        Self { packets }
    }

    pub fn new_with_recycler(
        recycler: PacketBatchRecycler<P>,
        size: usize,
        name: &'static str,
    ) -> Self {
        let mut packets = recycler.allocate(name);
        packets.reserve_and_pin(size);
        Self { packets }
    }

    pub fn new_with_recycler_data(
        recycler: &PacketBatchRecycler<P>,
        name: &'static str,
        mut packets: Vec<P>,
    ) -> Self {
        let mut batch = Self::new_with_recycler(recycler.clone(), packets.len(), name);
        batch.packets.append(&mut packets);
        batch
    }

    pub fn new_unpinned_with_recycler_data(
        recycler: &PacketBatchRecycler<P>,
        name: &'static str,
        mut packets: Vec<P>,
    ) -> Self {
        let mut batch = Self::new_unpinned_with_recycler(recycler.clone(), packets.len(), name);
        batch.packets.append(&mut packets);
        batch
    }

    pub fn set_addr(&mut self, addr: &SocketAddr) {
        for packet in self.packets.iter_mut() {
            packet.get_meta_mut().set_addr(addr);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }
}

pub fn to_packet_batches<T: Serialize, P: PacketInterface>(
    xs: &[T],
    chunks: usize,
) -> Vec<PacketBatch<P>> {
    let mut batches = vec![];
    for x in xs.chunks(chunks) {
        let mut batch = PacketBatch::with_capacity(x.len());
        batch.packets.resize(x.len(), P::default());
        for (i, packet) in x.iter().zip(batch.packets.iter_mut()) {
            P::populate_packet(packet, None, i).expect("serialize request");
        }
        batches.push(batch);
    }
    batches
}

#[cfg(test)]
pub fn to_packet_batches_for_tests<T: Serialize>(xs: &[T]) -> Vec<PacketBatch> {
    to_packet_batches(xs, NUM_PACKETS)
}

pub fn to_packet_batch_with_destination<T: Serialize, P: PacketInterface>(
    recycler: PacketBatchRecycler<P>,
    dests_and_data: &[(SocketAddr, T)],
) -> PacketBatch<P> {
    let mut out = PacketBatch::new_unpinned_with_recycler(
        recycler,
        dests_and_data.len(),
        "to_packet_batch_with_destination",
    );
    out.packets.resize(dests_and_data.len(), P::default());
    for (dest_and_data, o) in dests_and_data.iter().zip(out.packets.iter_mut()) {
        if !dest_and_data.0.ip().is_unspecified() && dest_and_data.0.port() != 0 {
            if let Err(e) = P::populate_packet(o, Some(&dest_and_data.0), &dest_and_data.1) {
                // TODO: This should never happen. Instead the caller should
                // break the payload into smaller messages, and here any errors
                // should be propagated.
                error!("Couldn't write to packet {:?}. Data skipped.", e);
            }
        } else {
            trace!("Dropping packet, as destination is unknown");
        }
    }
    out
}

// TODO (ryleung): Fix this to work with ExtendedPacket too
pub fn limited_deserialize<T>(data: &[u8]) -> bincode::Result<T>
where
    T: serde::de::DeserializeOwned,
{
    bincode::options()
        .with_limit(PACKET_DATA_SIZE as u64)
        .with_fixint_encoding()
        .allow_trailing_bytes()
        .deserialize_from(data)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        solana_sdk::{
            hash::Hash,
            signature::{Keypair, Signer},
            system_transaction,
        },
    };

    #[test]
    fn test_to_packet_batches() {
        let keypair = Keypair::new();
        let hash = Hash::new(&[1; 32]);
        let tx = system_transaction::transfer(&keypair, &keypair.pubkey(), 1, hash);
        let rv = to_packet_batches_for_tests(&[tx.clone(); 1]);
        assert_eq!(rv.len(), 1);
        assert_eq!(rv[0].packets.len(), 1);

        #[allow(clippy::useless_vec)]
        let rv = to_packet_batches_for_tests(&vec![tx.clone(); NUM_PACKETS]);
        assert_eq!(rv.len(), 1);
        assert_eq!(rv[0].packets.len(), NUM_PACKETS);

        #[allow(clippy::useless_vec)]
        let rv = to_packet_batches_for_tests(&vec![tx; NUM_PACKETS + 1]);
        assert_eq!(rv.len(), 2);
        assert_eq!(rv[0].packets.len(), NUM_PACKETS);
        assert_eq!(rv[1].packets.len(), 1);
    }

    #[test]
    fn test_to_packets_pinning() {
        let recycler = PacketBatchRecycler::default();
        for i in 0..2 {
            let _first_packets =
                PacketBatch::new_with_recycler(recycler.clone(), i + 1, "first one");
        }
    }
}

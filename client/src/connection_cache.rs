use {
    crate::{
        quic_client::QuicTpuConnection, tpu_connection::TpuConnection, udp_client::UdpTpuConnection,
    },
    lazy_static::lazy_static,
    lru::LruCache,
    solana_net_utils::VALIDATOR_PORT_RANGE,
    solana_sdk::{transaction::VersionedTransaction, transport::TransportError},
    std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::{Arc, Mutex},
    },
};

// Should be non-zero
static MAX_CONNECTIONS: usize = 64;

#[derive(Clone)]
enum Connection {
    Udp(Arc<UdpTpuConnection>),
    Quic(Arc<QuicTpuConnection>),
}

struct ConnMap {
    map: LruCache<SocketAddr, Connection>,
    use_quic: bool,
}

impl ConnMap {
    pub fn new() -> Self {
        Self {
            map: LruCache::new(MAX_CONNECTIONS),
            use_quic: false,
        }
    }

    pub fn set_use_quic(&mut self, use_quic: bool) {
        self.use_quic = use_quic;
    }
}

lazy_static! {
    static ref CONNECTION_MAP: Mutex<ConnMap> = Mutex::new(ConnMap::new());
}

pub fn set_use_quic(use_quic: bool) {
    let mut map = (*CONNECTION_MAP).lock().unwrap();
    map.set_use_quic(use_quic);
}

// TODO: see https://github.com/solana-labs/solana/issues/23661
// remove lazy_static and optimize and refactor this
fn get_connection(addr: &SocketAddr) -> Connection {
    let mut map = (*CONNECTION_MAP).lock().unwrap();

    match map.map.get(addr) {
        Some(connection) => connection.clone(),
        None => {
            let (_, send_socket) = solana_net_utils::bind_in_range(
                IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
                VALIDATOR_PORT_RANGE,
            )
            .unwrap();
            let connection = if map.use_quic {
                Connection::Quic(Arc::new(QuicTpuConnection::new(send_socket, *addr)))
            } else {
                Connection::Udp(Arc::new(UdpTpuConnection::new(send_socket, *addr)))
            };

            map.map.put(*addr, connection.clone());
            connection
        }
    }
}

// TODO: see https://github.com/solana-labs/solana/issues/23851
// use enum_dispatch and get rid of this tedious code.
// The main blocker to using enum_dispatch right now is that
// the it doesn't work with static methods like TpuConnection::new
// which is used by thin_client. This will be eliminated soon
// once thin_client is moved to using this connection cache.
// Once that is done, we will migrate to using enum_dispatch
// This will be done in a followup to
// https://github.com/solana-labs/solana/pull/23817
pub fn send_wire_transaction_batch(
    wire_transactions: &[&[u8]],
    addr: &SocketAddr,
) -> Result<(), TransportError> {
    let conn = get_connection(addr);
    match conn {
        Connection::Udp(conn) => conn.send_wire_transaction_batch(wire_transactions),
        Connection::Quic(conn) => conn.send_wire_transaction_batch(wire_transactions),
    }
}

pub fn send_wire_transaction_async(
    wire_transaction: &[u8],
    addr: &SocketAddr,
) -> Result<(), TransportError> {
    let conn = get_connection(addr);
    match conn {
        Connection::Udp(conn) => conn.send_wire_transaction_async(wire_transaction),
        Connection::Quic(conn) => conn.send_wire_transaction_async(wire_transaction),
    }
}

pub fn send_wire_transaction(
    wire_transaction: &[u8],
    addr: &SocketAddr,
) -> Result<(), TransportError> {
    let conn = get_connection(addr);
    match conn {
        Connection::Udp(conn) => conn.send_wire_transaction(wire_transaction),
        Connection::Quic(conn) => conn.send_wire_transaction(wire_transaction),
    }
}

pub fn serialize_and_send_transaction(
    transaction: &VersionedTransaction,
    addr: &SocketAddr,
) -> Result<(), TransportError> {
    let conn = get_connection(addr);
    match conn {
        Connection::Udp(conn) => conn.serialize_and_send_transaction(transaction),
        Connection::Quic(conn) => conn.serialize_and_send_transaction(transaction),
    }
}

pub fn par_serialize_and_send_transaction_batch(
    transactions: &[VersionedTransaction],
    addr: &SocketAddr,
) -> Result<(), TransportError> {
    let conn = get_connection(addr);
    match conn {
        Connection::Udp(conn) => conn.par_serialize_and_send_transaction_batch(transactions),
        Connection::Quic(conn) => conn.par_serialize_and_send_transaction_batch(transactions),
    }
}

#[cfg(test)]
mod tests {
    use {
        crate::{
            connection_cache::{get_connection, Connection, CONNECTION_MAP, MAX_CONNECTIONS},
            tpu_connection::TpuConnection,
        },
        rand::{Rng, SeedableRng},
        rand_chacha::ChaChaRng,
        std::net::{IpAddr, SocketAddr},
    };

    fn get_addr(rng: &mut ChaChaRng) -> SocketAddr {
        let a = rng.gen_range(1, 255);
        let b = rng.gen_range(1, 255);
        let c = rng.gen_range(1, 255);
        let d = rng.gen_range(1, 255);

        let addr_str = format!("{}.{}.{}.{}:80", a, b, c, d);

        addr_str.parse().expect("Invalid address")
    }

    fn ip(conn: Connection) -> IpAddr {
        match conn {
            Connection::Udp(conn) => conn.tpu_addr().ip(),
            Connection::Quic(conn) => conn.tpu_addr().ip(),
        }
    }

    #[test]
    fn test_connection_cache() {
        // Allow the test to run deterministically
        // with the same pseudorandom sequence between runs
        // and on different platforms - the cryptographic security
        // property isn't important here but ChaChaRng provides a way
        // to get the same pseudorandom sequence on different platforms
        let mut rng = ChaChaRng::seed_from_u64(42);

        // Generate a bunch of random addresses and create TPUConnections to them
        // Since TPUConnection::new is infallible, it should't matter whether or not
        // we can actually connect to those addresses - TPUConnection implementations should either
        // be lazy and not connect until first use or handle connection errors somehow
        // (without crashing, as would be required in a real practical validator)
        let first_addr = get_addr(&mut rng);
        assert!(ip(get_connection(&first_addr)) == first_addr.ip());
        let addrs = (0..MAX_CONNECTIONS)
            .into_iter()
            .map(|_| {
                let addr = get_addr(&mut rng);
                get_connection(&addr);
                addr
            })
            .collect::<Vec<_>>();
        {
            let map = (*CONNECTION_MAP).lock().unwrap();
            addrs.iter().for_each(|a| {
                let conn = map.map.peek(a).expect("Address not found");
                assert!(a.ip() == ip(conn.clone()));
            });

            assert!(map.map.peek(&first_addr).is_none());
        }

        // Test that get_connection updates which connection is next up for eviction
        // when an existing connection is used. Initially, addrs[0] should be next up for eviction, since
        // it was the earliest added. But we do get_connection(&addrs[0]), thereby using
        // that connection, and bumping it back to the end of the queue. So addrs[1] should be
        // the next up for eviction. So we add a new connection, and test that addrs[0] is not
        // evicted but addrs[1] is.
        get_connection(&addrs[0]);
        get_connection(&get_addr(&mut rng));

        let map = (*CONNECTION_MAP).lock().unwrap();
        assert!(map.map.peek(&addrs[0]).is_some());
        assert!(map.map.peek(&addrs[1]).is_none());
    }
}

//! Tor, through a SOCKS5 proxy.
//!
//! Sieve does not ship or manage a Tor daemon; it uses one already running on
//! the machine — the system service on `127.0.0.1:9050`, or Tor Browser's on
//! `9150`. That is the same arrangement Bitcoin Core and Sparrow use, and it
//! keeps the thing that needs careful updating out of a wallet's release
//! cycle.
//!
//! Two jobs here beyond pointing the node at a proxy:
//!
//! **Proving it is really Tor.** Anything can listen on 9050. `RESOLVE` (0xF0)
//! is a Tor extension to SOCKS5, not part of RFC 1928, so a plain SOCKS proxy
//! answers "command not supported" and only Tor answers with an address. That
//! makes one round trip both the check and the useful work.
//!
//! **Not leaking DNS.** kyoto falls back to a DNS lookup when it has no peers
//! to try, and that lookup goes out over the clear — it is not routed through
//! the proxy, and since kyoto's peer database is in memory it happens on every
//! launch. So the seeds are resolved *here*, through Tor, and handed to the
//! node as configured peers. The resolver never learns this machine is looking
//! for Bitcoin nodes, because the query comes out of an exit relay.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};

pub mod daemon;
pub mod onion;

/// Where Tor usually listens. The system daemon first, then Tor Browser's.
pub const PORTS: [u16; 2] = [9050, 9150];

const TIMEOUT: Duration = Duration::from_secs(10);

const VERSION: u8 = 5;
const NO_AUTH: u8 = 0;
/// Tor's SOCKS extension, not RFC 1928. Its absence is what proves a proxy is
/// something other than Tor.
const CMD_RESOLVE: u8 = 0xF0;
const ADDR_IPV4: u8 = 1;
const ADDR_DOMAIN: u8 = 3;
const ADDR_IPV6: u8 = 4;
const REPLY_OK: u8 = 0;

/// The address of a SOCKS5 proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proxy(pub SocketAddr);

impl Proxy {
    pub fn local(port: u16) -> Self {
        Proxy(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
    }

    pub fn addr(&self) -> SocketAddr {
        self.0
    }

    /// As ureq wants it. `socks5h`, not `socks5`: the *h* is what makes the
    /// proxy resolve hostnames instead of this machine doing it and handing
    /// the answer over — which would leak every lookup Tor is there to hide.
    pub fn ureq_url(&self) -> String {
        format!("socks5h://{}", self.0)
    }
}

impl std::fmt::Display for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for Proxy {
    type Err = anyhow::Error;

    /// Accepts `host:port`, or a bare port, or a bare host on the usual port.
    fn from_str(text: &str) -> Result<Self> {
        let text = text.trim();
        if let Ok(port) = text.parse::<u16>() {
            return Ok(Proxy::local(port));
        }
        if let Ok(addr) = text.parse::<SocketAddr>() {
            return Ok(Proxy(addr));
        }
        if let Ok(ip) = text.parse::<IpAddr>() {
            return Ok(Proxy(SocketAddr::new(ip, PORTS[0])));
        }
        bail!("that is not a proxy address — try 127.0.0.1:9050")
    }
}

/// The proxy as ureq wants it, or `None` for a direct connection.
///
/// Its own function so that the two HTTP calls cannot accidentally be made
/// without consulting the setting: they both take an `Option<Proxy>` and hand
/// it here.
pub fn ureq_proxy(proxy: Option<Proxy>) -> Result<Option<ureq::Proxy>> {
    match proxy {
        Some(proxy) => {
            Ok(Some(ureq::Proxy::new(&proxy.ureq_url()).map_err(|e| {
                anyhow!("could not use the proxy at {proxy}: {e}")
            })?))
        }
        None => Ok(None),
    }
}

/// Find a Tor proxy on the usual ports, verifying each before believing it.
pub fn detect() -> Option<Proxy> {
    PORTS
        .iter()
        .map(|port| Proxy::local(*port))
        .find(|proxy| check(*proxy).is_ok())
}

/// Confirm the proxy is answering, and that it is Tor.
///
/// Blocking. Returns the address a well-known hostname resolved to, which is
/// evidence that the lookup went through the network rather than this machine.
pub fn check(proxy: Proxy) -> Result<IpAddr> {
    // Deliberately not a Bitcoin seed: this runs whenever someone opens the
    // preferences, and it should say nothing about what the app is for.
    resolve(proxy, "example.com")
}

/// Resolve a hostname through Tor, using its `RESOLVE` SOCKS extension.
///
/// Blocking: call it from a command, never on the main thread.
pub fn resolve(proxy: Proxy, host: &str) -> Result<IpAddr> {
    let mut stream = TcpStream::connect_timeout(&proxy.addr(), TIMEOUT)
        .map_err(|e| anyhow!("nothing is answering at {proxy}: {e}"))?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.set_nodelay(true)?;

    // Greeting: one method offered, no authentication.
    stream.write_all(&[VERSION, 1, NO_AUTH])?;
    let mut greeting = [0u8; 2];
    stream.read_exact(&mut greeting)?;
    if greeting[0] != VERSION {
        bail!("{proxy} is not a SOCKS5 proxy");
    }
    if greeting[1] != NO_AUTH {
        bail!("{proxy} wants authentication, which Sieve does not send");
    }

    stream.write_all(&resolve_request(host)?)?;

    let mut head = [0u8; 4];
    stream.read_exact(&mut head)?;
    if head[1] != REPLY_OK {
        // 0x07 is "command not supported", which is precisely what a SOCKS5
        // proxy that is not Tor says to RESOLVE.
        if head[1] == 0x07 {
            bail!("{proxy} is a SOCKS5 proxy but not Tor");
        }
        bail!("Tor could not resolve {host} (SOCKS reply {})", head[1]);
    }

    match head[3] {
        ADDR_IPV4 => {
            let mut octets = [0u8; 4];
            stream.read_exact(&mut octets)?;
            Ok(IpAddr::from(octets))
        }
        ADDR_IPV6 => {
            let mut octets = [0u8; 16];
            stream.read_exact(&mut octets)?;
            Ok(IpAddr::from(octets))
        }
        other => bail!("Tor answered with an address type Sieve does not read ({other})"),
    }
}

/// The bytes of a `RESOLVE` request.
///
/// Its own function so the wire format can be tested without a proxy.
fn resolve_request(host: &str) -> Result<Vec<u8>> {
    let bytes = host.as_bytes();
    if bytes.is_empty() || bytes.len() > 255 {
        bail!("that hostname cannot go in a SOCKS request");
    }

    let mut request = Vec::with_capacity(bytes.len() + 7);
    request.extend_from_slice(&[VERSION, CMD_RESOLVE, 0, ADDR_DOMAIN]);
    request.push(bytes.len() as u8);
    request.extend_from_slice(bytes);
    // Port is meaningless for a resolution, and Tor ignores it.
    request.extend_from_slice(&[0, 0]);
    Ok(request)
}

/// The DNS seeds for a network, with the service-bit prefixes that ask a
/// seeder for nodes serving compact filters.
///
/// The same hostnames kyoto would have looked up itself, resolved here so the
/// query goes through Tor instead of the machine's resolver. `x49` is the
/// filter-serving service bits; a seeder that understands the prefix returns
/// only those nodes, and one that does not returns ordinary nodes, which is no
/// worse than what we would have had.
pub fn seeds(network: &str) -> Vec<String> {
    let hosts: &[&str] = match network {
        "bitcoin" => &[
            "seed.bitcoin.sipa.be",
            "dnsseed.bluematt.me",
            "seed.bitcoinstats.com",
            "seed.bitcoin.jonasschnelli.ch",
            "seed.btc.petertodd.org",
            "seed.bitcoin.sprovoost.nl",
            "dnsseed.emzy.de",
            "seed.bitcoin.wiz.biz",
        ],
        "signet" => &[
            "seed.dlsouza.lol",
            "seed.signet.bitcoin.sprovoost.nl",
            "seed.signet.achownodes.xyz",
        ],
        "testnet" => &[
            "testnet-seed.bitcoin.jonasschnelli.ch",
            "seed.tbtc.petertodd.org",
            "seed.testnet.bitcoin.sprovoost.nl",
        ],
        "testnet4" => &[
            "seed.testnet4.bitcoin.sprovoost.nl",
            "seed.testnet4.wiz.biz",
        ],
        // A chain on this machine has no seeds, and Tor has nothing to hide
        // about a connection to localhost.
        _ => &[],
    };

    // Filter-serving nodes only. A peer that cannot serve compact filters is
    // worse than no peer at all here: it takes a connection slot, and this
    // wallet's entire sync is filters. Asking the plain hostnames as a
    // fallback filled the slots with ordinary nodes and stalled the download
    // at two thousand filters of two hundred thousand.
    //
    // `x49` is NODE_NETWORK | NODE_COMPACT_FILTERS; `x849` adds BIP324 v2
    // transport, which kyoto disables over Tor but whose operators are the
    // sort who run filter indexes.
    let mut names = Vec::with_capacity(hosts.len() * 2);
    names.extend(hosts.iter().map(|host| format!("x49.{host}")));
    names.extend(hosts.iter().map(|host| format!("x849.{host}")));
    names
}

/// Resolve enough seeds through Tor to start a node with.
///
/// Tor's `RESOLVE` returns one address per lookup, where an ordinary DNS query
/// would return a dozen — so this asks several seeders rather than one, and
/// takes what each gives. Every lookup is a round trip through three relays,
/// which is why it stops as soon as it has enough.
pub fn resolve_seeds(proxy: Proxy, network: &str, wanted: usize) -> Vec<IpAddr> {
    let mut found = Vec::new();
    // Each hostname asked more than once: `RESOLVE` returns a single address
    // where an ordinary DNS query returns a dozen, and a seeder answers
    // differently each time. Two passes over eight seeders is sixteen chances
    // at a filter-serving node, for sixteen quick round trips.
    for _ in 0..ASKS_PER_SEED {
        for host in seeds(network) {
            if found.len() >= wanted {
                return found;
            }
            match resolve(proxy, &host) {
                Ok(ip) if !found.contains(&ip) => {
                    tracing::debug!(%host, %ip, "resolved a filter-serving seed through Tor");
                    found.push(ip);
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(%host, %e, "a seed did not resolve through Tor"),
            }
        }
    }
    found
}

/// How many times to ask each seeder. Tor answers one address per lookup, and
/// a seeder picks a different node each time it is asked.
const ASKS_PER_SEED: usize = 3;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn a_resolve_request_is_shaped_the_way_tor_expects() {
        let request = resolve_request("example.com").unwrap();
        assert_eq!(request[0], 5, "SOCKS version");
        assert_eq!(request[1], 0xF0, "RESOLVE, Tor's extension");
        assert_eq!(request[3], 3, "the target is a domain name");
        assert_eq!(request[4] as usize, "example.com".len());
        assert_eq!(&request[5..5 + 11], b"example.com");
        assert_eq!(&request[request.len() - 2..], &[0, 0], "port is unused");
    }

    #[test]
    fn hostnames_that_cannot_be_sent_are_refused() {
        assert!(resolve_request("").is_err());
        assert!(resolve_request(&"a".repeat(256)).is_err());
    }

    #[test]
    fn proxies_can_be_written_several_ways() {
        assert_eq!("9050".parse::<Proxy>().unwrap(), Proxy::local(9050));
        assert_eq!(
            "127.0.0.1:9150".parse::<Proxy>().unwrap(),
            Proxy::local(9150)
        );
        assert_eq!("127.0.0.1".parse::<Proxy>().unwrap(), Proxy::local(9050));
        assert!("not a proxy".parse::<Proxy>().is_err());
        // The h matters: without it the hostname is resolved here, which is
        // the leak the proxy exists to prevent.
        assert!(Proxy::local(9050).ureq_url().starts_with("socks5h://"));
    }

    #[test]
    fn every_network_asks_its_own_seeds_and_prefers_filter_nodes() {
        let mainnet = seeds("bitcoin");
        assert!(mainnet.iter().any(|h| h == "x49.seed.bitcoin.sipa.be"));
        assert!(mainnet.iter().any(|h| h == "x849.seed.bitcoin.sipa.be"));
        // Never the bare hostname: it answers with ordinary nodes, which take
        // a connection slot and cannot serve a single filter.
        assert!(
            !mainnet.iter().any(|h| h == "seed.bitcoin.sipa.be"),
            "plain seeds return peers that cannot serve filters"
        );
        assert!(!seeds("signet").is_empty());
        assert!(seeds("regtest").is_empty(), "a local chain has no seeds");
        // Signet seeds on mainnet would be a slow way to find nothing.
        assert!(!seeds("bitcoin").iter().any(|h| h.contains("signet")));
    }

    /// A stand-in proxy that speaks just enough SOCKS5 to answer, so the
    /// client end is exercised for real without a Tor daemon.
    fn fake_proxy(reply: Vec<u8>) -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut greeting = [0u8; 3];
            stream.read_exact(&mut greeting).unwrap();
            stream.write_all(&[5, 0]).unwrap();
            let mut head = [0u8; 5];
            stream.read_exact(&mut head).unwrap();
            let mut host = vec![0u8; head[4] as usize + 2];
            stream.read_exact(&mut host).unwrap();
            stream.write_all(&reply).unwrap();
        });
        Proxy(addr)
    }

    #[test]
    fn an_answer_from_tor_is_read_back() {
        let proxy = fake_proxy(vec![5, 0, 0, 1, 93, 184, 216, 34, 0, 0]);
        assert_eq!(
            resolve(proxy, "example.com").unwrap(),
            IpAddr::from([93, 184, 216, 34])
        );
    }

    /// The check that keeps "Tor is on" from meaning "something is listening
    /// on 9050".
    #[test]
    fn a_socks_proxy_that_is_not_tor_is_rejected() {
        // 0x07: command not supported, which is what RESOLVE gets from a
        // proxy that only implements RFC 1928.
        let proxy = fake_proxy(vec![5, 7, 0, 1, 0, 0, 0, 0, 0, 0]);
        let error = resolve(proxy, "example.com").unwrap_err().to_string();
        assert!(error.contains("not Tor"), "{error}");
    }

    #[test]
    fn nothing_listening_is_an_error_not_a_hang() {
        // Port 1 on loopback: nothing is there, and connect fails at once.
        let error = resolve(Proxy::local(1), "example.com")
            .unwrap_err()
            .to_string();
        assert!(error.contains("nothing is answering"), "{error}");
    }
}

//! # ssdp
//!
//! `ssdp` implements the [Simple Service Discovery
//! Protocol](https://datatracker.ietf.org/doc/html/draft-cai-ssdp-v1-03),
//! responding to UDP discovery requests to the multicast address
//! 239.255.255.250:1900 and emitting notifications on the same address.
use std::net::{Ipv4Addr, SocketAddrV4};
use std::io;

use nix::sys::socket::LinkAddr;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;
use tracing::{warn, info, debug, trace};

const SSDP_MCAST: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const SSDP_PORT: u16 = 1900;
const SSDP_ST: &[u8] = b"meliza.org:decide";
const SSDP_DISC: &[u8] = b"\"ssdp:discover\"";
const SSDP_ALL: &[u8] = b"ssdp:all";
const SSDP_MAX_AGE: u64 = 60;

/// Get the IPv4 address for a given interface
fn get_interface_ip(interface: &str) -> Option<Ipv4Addr> {
    let addrs = nix::ifaddrs::getifaddrs().unwrap();
    addrs
	// with the right name
	.filter(|x| x.interface_name == interface)
	// that are valid addresses
	.filter_map(|x| x.address)
	// and can be downcast to a IPv4 address
	.filter_map(|x| x.as_sockaddr_in().and_then(|x| Some(Ipv4Addr::from(x.ip()))))
	.next()
}

/// Get the MAC address for a given interface
fn get_interface_mac(interface: &str) -> Option<LinkAddr> {
    let addrs = nix::ifaddrs::getifaddrs().unwrap();
    addrs
	// that have the right name
	.filter(|x| x.interface_name == interface)
	// that are valid addresses
	.filter_map(|x| x.address)
	// and can be downcast to a mac
	.filter_map(|x| x.as_link_addr().copied())
	.next()
}

/// Create a new non-blocking UDP socket for ipv4
fn new_socket() -> io::Result<Socket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_nonblocking(true)?; // tokio wants non-blocking sockets
    Ok(socket)
}

/// Create a new UPD socket, joins it to the SSDP multicast address using the
/// specified interface, and bind to 0.0.0.0
fn join_multicast(interface_address: Ipv4Addr) -> io::Result<std::net::UdpSocket> {
    let socket = new_socket()?;
    trace!("joining {} with {}", SSDP_MCAST, interface_address);
    socket.join_multicast_v4(&SSDP_MCAST, &interface_address)?;
    socket.set_multicast_ttl_v4(2)?;
    let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, SSDP_PORT);
    socket.bind(&bind_addr.into())?;
    Ok(socket.into())
}

/// Returns true if a message is an SSDP discovery message that matches our ST
fn is_discovery(data: &[u8]) -> bool {
    let mut headers = [httparse::EMPTY_HEADER; 16];
    let mut req = httparse::Request::new(&mut headers);
    let res = req.parse(data);
    if res.is_ok() && req.method == Some("M-SEARCH") && req.path == Some("*") {
        let has_man = req
            .headers
            .iter()
            .any(|x| x.name.eq_ignore_ascii_case("man") && x.value == SSDP_DISC);
        let has_st = req.headers.iter().any(|x| {
            x.name.eq_ignore_ascii_case("st") && (x.value == SSDP_ST || x.value == SSDP_ALL)
        });
        has_man && has_st
    } else {
        false
    }
}

/// Listen to SSDP address and reply to discovery requests
pub async fn listener(interface: &str) -> io::Result<()> {
    let my_address = get_interface_ip(interface).expect("unable to determine interface ip");
    let my_mac = get_interface_mac(interface).expect("unable to determine interface MAC");
    let sock = join_multicast(my_address).expect("failed to bind multicast socket");
    let sock = UdpSocket::from_std(sock).unwrap();
    let mut buf = [0; 2048];
    let reply = format!(
        "HTTP/1.1 200 OK\r\nST: {}\r\nUSN: {}\r\nCache-Control: max-age={}\r\n\r\n",
        String::from_utf8_lossy(SSDP_ST),
        my_mac,
        SSDP_MAX_AGE,
    );
    let reply = reply.as_bytes();
    info!(
        "listening for ssdp:discovery: {}:{} -> {}",
        SSDP_MCAST, SSDP_PORT, my_address
    );
    loop {
        match sock.recv_from(&mut buf).await {
            Ok((len, src_addr)) => {
                let data = &buf[..len];
                //println!("{:?}", String::from_utf8_lossy(data));
                if is_discovery(data) {
                    debug!("discovery request from {}", src_addr);
                    sock.send_to(&reply, src_addr).await?;
                }
            }
            Err(err) => {
                warn!("listener error: {}", err);
            }
        }
    }
}

/// Send SSDP notifications at periodic intervals. When cancel_token is
/// cancelled, emits ssdp::byebye before shutting down
pub async fn notifier(interface: &str, cancel_token: CancellationToken) -> io::Result<()> {
    let my_address = get_interface_ip(interface).expect("unable to determine interface ip");
    let my_mac = get_interface_mac(interface).expect("unable to determine interface MAC");
    let src_address = SocketAddrV4::new(my_address, 0);
    let sock = new_socket().unwrap();
    sock.bind(&src_address.into())?;
    let sock = UdpSocket::from_std(sock.into()).unwrap();
    let dest_address = SocketAddrV4::new(SSDP_MCAST, SSDP_PORT);
    info!("multicasting ssdp notifications: {} -> {}", my_address, dest_address);
    let msg = format!("NOTIFY * HTTP/1.1\r\nHost: {}\r\nCache-Control: max-age={}\r\nNT: {}\r\nNTS: ssdp:alive\r\nUSN: {}\r\n\r\n",
		      dest_address,
		      SSDP_MAX_AGE,
		      String::from_utf8_lossy(SSDP_ST),
		      my_mac);
    let msg = msg.as_bytes();

    let mut to_continue: bool = true;
    while to_continue {
        match sock.send_to(&msg, &dest_address).await {
            Ok(_) => {
                debug!("sent ssdp:notify to {}", dest_address);
            }
            Err(err) => {
                warn!("notifier error: {}", err)
            }
        }
        to_continue = tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(SSDP_MAX_AGE)) => {true},
            _ = cancel_token.cancelled() => {false},
        };
    }
    let msg = format!(
        "NOTIFY * HTTP/1.1\r\nHost: {}\r\nNT: {}\r\nNTS: ssdp:byebye\r\nUSN: {}\r\n\r\n",
        dest_address,
        String::from_utf8_lossy(SSDP_ST),
        my_mac
    );
    let msg = msg.as_bytes();
    if let Ok(_) = sock.send_to(&msg, &dest_address).await {
        println!("- sent ssdp:byebye to {}", dest_address);
    }
    Ok(())
}

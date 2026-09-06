use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use anyhow::{bail, Context, Result};
use rand::Rng;
use tokio::net::UdpSocket;
use tokio::time::{timeout, Duration};
use crate::bencode::{self, BencodeValue};
use crate::torrent::Torrent;

pub fn generate_peer_id() -> [u8; 20] {
    let mut peer_id = [0u8; 20];
    let prefix = b"-RS0001-";
    peer_id[..prefix.len()].copy_from_slice(prefix);
    let mut rng = rand::thread_rng();
    for byte in &mut peer_id[prefix.len()..] {
        *byte = rng.gen_range(b'0'..=b'z');
    }
    peer_id
}

pub fn urlencode_binary(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 3);
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            result.push(b as char);
        } else {
            result.push_str(&format!("%{:02X}", b));
        }
    }
    result
}

pub struct TrackerResponse {
    pub interval: usize,
    pub peers: Vec<SocketAddr>,
}

pub async fn get_peers(torrent: &Torrent, peer_id: &[u8; 20], port: u16) -> Result<Vec<SocketAddr>> {
    let mut all_peers = Vec::new();
    let mut last_err = None;

    // Try trackers in announce_list
    for tracker_url in &torrent.announce_list {
        let res = if tracker_url.starts_with("http://") || tracker_url.starts_with("https://") {
            announce_http(tracker_url, torrent, peer_id, port).await
        } else if tracker_url.starts_with("udp://") {
            announce_udp(tracker_url, torrent, peer_id, port).await
        } else {
            continue;
        };

        match res {
            Ok(resp) => {
                for peer in resp.peers {
                    if !all_peers.contains(&peer) {
                        all_peers.push(peer);
                    }
                }
                if !all_peers.is_empty() {
                    return Ok(all_peers);
                }
            }
            Err(e) => {
                last_err = Some(e);
            }
        }
    }

    if !all_peers.is_empty() {
        Ok(all_peers)
    } else if let Some(e) = last_err {
        Err(e)
    } else {
        bail!("No valid peers found from trackers")
    }
}

pub async fn announce_http(
    tracker_url: &str,
    torrent: &Torrent,
    peer_id: &[u8; 20],
    port: u16,
) -> Result<TrackerResponse> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let sep = if tracker_url.contains('?') { '&' } else { '?' };
    let query_url = format!(
        "{}{}info_hash={}&peer_id={}&port={}&uploaded=0&downloaded=0&left={}&compact=1",
        tracker_url,
        sep,
        urlencode_binary(&torrent.info_hash),
        urlencode_binary(peer_id),
        port,
        torrent.length,
    );

    let response = client
        .get(&query_url)
        .send()
        .await
        .context("Failed to send HTTP request to tracker")?;

    let bytes = response
        .bytes()
        .await
        .context("Failed to read tracker response body")?;

    let decoded = bencode::decode(&bytes).context("Failed to parse tracker bencode response")?;
    let dict = decoded.as_dict().context("Tracker response is not a dict")?;

    if let Some(reason) = dict.get(b"failure reason".as_ref()).and_then(|v| v.as_str()) {
        bail!("Tracker responded with failure: {}", reason);
    }

    let interval = dict
        .get(b"interval".as_ref())
        .and_then(|v| v.as_int())
        .unwrap_or(900) as usize;

    let mut peers = Vec::new();
    if let Some(peers_val) = dict.get(b"peers".as_ref()) {
        match peers_val {
            BencodeValue::ByteString(bytes) => {
                // Compact format: 6 bytes per peer (4 IP, 2 Port)
                for chunk in bytes.chunks_exact(6) {
                    let ip = Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]);
                    let port = u16::from_be_bytes([chunk[4], chunk[5]]);
                    peers.push(SocketAddr::V4(SocketAddrV4::new(ip, port)));
                }
            }
            BencodeValue::List(peer_list) => {
                // Dict format
                for p in peer_list {
                    if let Some(p_dict) = p.as_dict() {
                        let ip_str = p_dict.get(b"ip".as_ref()).and_then(|v| v.as_str());
                        let port_num = p_dict.get(b"port".as_ref()).and_then(|v| v.as_int());
                        if let (Some(ip_str), Some(port_num)) = (ip_str, port_num) {
                            if let Ok(ip) = ip_str.parse::<Ipv4Addr>() {
                                peers.push(SocketAddr::V4(SocketAddrV4::new(ip, port_num as u16)));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(TrackerResponse { interval, peers })
}

pub async fn announce_udp(
    tracker_url: &str,
    torrent: &Torrent,
    peer_id: &[u8; 20],
    port: u16,
) -> Result<TrackerResponse> {
    let parsed_url = url::Url::parse(tracker_url)?;
    let host = parsed_url.host_str().context("No host in UDP tracker URL")?;
    let tracker_port = parsed_url.port().unwrap_or(80);
    let addr_str = format!("{}:{}", host, tracker_port);

    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(&addr_str).await?;

    let mut rng = rand::thread_rng();

    // Step 1: Connect Request
    // Protocol ID: 0x41727101980 (64-bit int)
    // Action: 0 (connect, 32-bit int)
    // Transaction ID: random 32-bit int
    let transaction_id: u32 = rng.gen();
    let mut connect_req = [0u8; 16];
    connect_req[..8].copy_from_slice(&0x41727101980u64.to_be_bytes());
    connect_req[8..12].copy_from_slice(&0u32.to_be_bytes()); // action = 0
    connect_req[12..16].copy_from_slice(&transaction_id.to_be_bytes());

    socket.send(&connect_req).await?;

    let mut buf = [0u8; 1024];
    let n = timeout(Duration::from_secs(5), socket.recv(&mut buf))
        .await
        .context("UDP tracker connection timed out")??;

    if n < 16 {
        bail!("UDP tracker connection response too short");
    }

    let action = u32::from_be_bytes(buf[0..4].try_into()?);
    let resp_trans_id = u32::from_be_bytes(buf[4..8].try_into()?);
    if action != 0 || resp_trans_id != transaction_id {
        bail!("Invalid UDP connect response");
    }

    let connection_id = &buf[8..16];

    // Step 2: Announce Request
    let announce_trans_id: u32 = rng.gen();
    let mut announce_req = Vec::with_capacity(98);
    announce_req.extend_from_slice(connection_id); // connection_id (8 bytes)
    announce_req.extend_from_slice(&1u32.to_be_bytes()); // action = 1 (announce)
    announce_req.extend_from_slice(&announce_trans_id.to_be_bytes()); // transaction_id
    announce_req.extend_from_slice(&torrent.info_hash); // info_hash (20 bytes)
    announce_req.extend_from_slice(peer_id); // peer_id (20 bytes)
    announce_req.extend_from_slice(&0u64.to_be_bytes()); // downloaded
    announce_req.extend_from_slice(&(torrent.length as u64).to_be_bytes()); // left
    announce_req.extend_from_slice(&0u64.to_be_bytes()); // uploaded
    announce_req.extend_from_slice(&0u32.to_be_bytes()); // event (0 = none)
    announce_req.extend_from_slice(&0u32.to_be_bytes()); // IP address (0 = default)
    announce_req.extend_from_slice(&rng.gen::<u32>().to_be_bytes()); // key
    announce_req.extend_from_slice(&(-1i32).to_be_bytes()); // num_want (-1 = default)
    announce_req.extend_from_slice(&port.to_be_bytes()); // port

    socket.send(&announce_req).await?;

    let n = timeout(Duration::from_secs(5), socket.recv(&mut buf))
        .await
        .context("UDP tracker announce timed out")??;

    if n < 20 {
        bail!("UDP tracker announce response too short");
    }

    let action = u32::from_be_bytes(buf[0..4].try_into()?);
    let resp_trans_id = u32::from_be_bytes(buf[4..8].try_into()?);
    if action != 1 || resp_trans_id != announce_trans_id {
        bail!("Invalid UDP announce response");
    }

    let interval = u32::from_be_bytes(buf[8..12].try_into()?) as usize;
    // buf[12..16] is leechers, buf[16..20] is seeders
    let peer_bytes = &buf[20..n];

    let mut peers = Vec::new();
    for chunk in peer_bytes.chunks_exact(6) {
        let ip = Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]);
        let port = u16::from_be_bytes([chunk[4], chunk[5]]);
        peers.push(SocketAddr::V4(SocketAddrV4::new(ip, port)));
    }

    Ok(TrackerResponse { interval, peers })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_peer_id_generation() {
        let id1 = generate_peer_id();
        let id2 = generate_peer_id();
        assert_eq!(&id1[..8], b"-RS0001-");
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_urlencode_binary() {
        let hash = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x12, 0x34, 0x56, 0x78];
        let encoded = urlencode_binary(&hash);
        assert!(encoded.contains("%12"));
        assert!(encoded.contains("%9A") || encoded.contains("%9a"));
    }
}

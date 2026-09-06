use std::net::SocketAddr;
use anyhow::{bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};

pub const HANDSHAKE_PSTR: &[u8] = b"BitTorrent protocol";
pub const STANDARD_BLOCK_SIZE: usize = 16 * 1024; // 16 KiB

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
}

impl Handshake {
    pub fn new(info_hash: [u8; 20], peer_id: [u8; 20]) -> Self {
        Self { info_hash, peer_id }
    }

    pub fn to_bytes(&self) -> [u8; 68] {
        let mut buf = [0u8; 68];
        buf[0] = 19; // length of "BitTorrent protocol"
        buf[1..20].copy_from_slice(HANDSHAKE_PSTR);
        buf[20..28].copy_from_slice(&[0u8; 8]); // 8 reserved bytes
        buf[28..48].copy_from_slice(&self.info_hash);
        buf[48..68].copy_from_slice(&self.peer_id);
        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 68 {
            bail!("Handshake buffer too short: {} bytes", bytes.len());
        }
        if bytes[0] != 19 || &bytes[1..20] != HANDSHAKE_PSTR {
            bail!("Invalid protocol string in handshake");
        }
        let mut info_hash = [0u8; 20];
        let mut peer_id = [0u8; 20];
        info_hash.copy_from_slice(&bytes[28..48]);
        peer_id.copy_from_slice(&bytes[48..68]);
        Ok(Self { info_hash, peer_id })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    KeepAlive,
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have(u32),
    Bitfield(Vec<u8>),
    Request { index: u32, begin: u32, length: u32 },
    Piece { index: u32, begin: u32, block: Vec<u8> },
    Cancel { index: u32, begin: u32, length: u32 },
}

impl Message {
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Message::KeepAlive => vec![0, 0, 0, 0],
            Message::Choke => vec![0, 0, 0, 1, 0],
            Message::Unchoke => vec![0, 0, 0, 1, 1],
            Message::Interested => vec![0, 0, 0, 1, 2],
            Message::NotInterested => vec![0, 0, 0, 1, 3],
            Message::Have(index) => {
                let mut buf = Vec::with_capacity(9);
                buf.extend_from_slice(&5u32.to_be_bytes());
                buf.push(4);
                buf.extend_from_slice(&index.to_be_bytes());
                buf
            }
            Message::Bitfield(bitfield) => {
                let mut buf = Vec::with_capacity(5 + bitfield.len());
                let len = 1 + bitfield.len() as u32;
                buf.extend_from_slice(&len.to_be_bytes());
                buf.push(5);
                buf.extend_from_slice(bitfield);
                buf
            }
            Message::Request { index, begin, length } => {
                let mut buf = Vec::with_capacity(17);
                buf.extend_from_slice(&13u32.to_be_bytes());
                buf.push(6);
                buf.extend_from_slice(&index.to_be_bytes());
                buf.extend_from_slice(&begin.to_be_bytes());
                buf.extend_from_slice(&length.to_be_bytes());
                buf
            }
            Message::Piece { index, begin, block } => {
                let mut buf = Vec::with_capacity(13 + block.len());
                let len = 9 + block.len() as u32;
                buf.extend_from_slice(&len.to_be_bytes());
                buf.push(7);
                buf.extend_from_slice(&index.to_be_bytes());
                buf.extend_from_slice(&begin.to_be_bytes());
                buf.extend_from_slice(block);
                buf
            }
            Message::Cancel { index, begin, length } => {
                let mut buf = Vec::with_capacity(17);
                buf.extend_from_slice(&13u32.to_be_bytes());
                buf.push(8);
                buf.extend_from_slice(&index.to_be_bytes());
                buf.extend_from_slice(&begin.to_be_bytes());
                buf.extend_from_slice(&length.to_be_bytes());
                buf
            }
        }
    }
}

pub struct PeerBitfield {
    bytes: Vec<u8>,
}

impl PeerBitfield {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    pub fn has_piece(&self, index: usize) -> bool {
        let byte_idx = index / 8;
        let bit_idx = 7 - (index % 8);
        if byte_idx < self.bytes.len() {
            (self.bytes[byte_idx] & (1 << bit_idx)) != 0
        } else {
            false
        }
    }

    pub fn set_piece(&mut self, index: usize) {
        let byte_idx = index / 8;
        let bit_idx = 7 - (index % 8);
        if byte_idx >= self.bytes.len() {
            self.bytes.resize(byte_idx + 1, 0);
        }
        self.bytes[byte_idx] |= 1 << bit_idx;
    }
}

pub struct PeerConnection {
    pub stream: TcpStream,
    pub peer_id: [u8; 20],
    pub choked: bool,
    pub bitfield: PeerBitfield,
}

impl PeerConnection {
    pub async fn connect(
        addr: SocketAddr,
        info_hash: [u8; 20],
        client_peer_id: [u8; 20],
        connect_timeout: Duration,
    ) -> Result<Self> {
        let mut stream = timeout(connect_timeout, TcpStream::connect(addr))
            .await
            .context("Peer connection timed out")??;

        // Perform Handshake
        let hs = Handshake::new(info_hash, client_peer_id);
        stream.write_all(&hs.to_bytes()).await?;

        let mut hs_buf = [0u8; 68];
        timeout(connect_timeout, stream.read_exact(&mut hs_buf))
            .await
            .context("Peer handshake read timed out")??;

        let peer_hs = Handshake::from_bytes(&hs_buf)?;
        if peer_hs.info_hash != info_hash {
            bail!("Peer returned mismatched info_hash");
        }

        Ok(Self {
            stream,
            peer_id: peer_hs.peer_id,
            choked: true,
            bitfield: PeerBitfield::new(Vec::new()),
        })
    }

    pub async fn send_message(&mut self, msg: &Message) -> Result<()> {
        let bytes = msg.to_bytes();
        self.stream.write_all(&bytes).await?;
        Ok(())
    }

    pub async fn read_message(&mut self) -> Result<Message> {
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;

        if len == 0 {
            return Ok(Message::KeepAlive);
        }

        let mut payload = vec![0u8; len];
        self.stream.read_exact(&mut payload).await?;

        let id = payload[0];
        let data = &payload[1..];

        let msg = match id {
            0 => Message::Choke,
            1 => Message::Unchoke,
            2 => Message::Interested,
            3 => Message::NotInterested,
            4 => {
                if data.len() < 4 {
                    bail!("Invalid Have message length");
                }
                let idx = u32::from_be_bytes(data[0..4].try_into()?);
                Message::Have(idx)
            }
            5 => Message::Bitfield(data.to_vec()),
            6 => {
                if data.len() < 12 {
                    bail!("Invalid Request message length");
                }
                let index = u32::from_be_bytes(data[0..4].try_into()?);
                let begin = u32::from_be_bytes(data[4..8].try_into()?);
                let length = u32::from_be_bytes(data[8..12].try_into()?);
                Message::Request { index, begin, length }
            }
            7 => {
                if data.len() < 8 {
                    bail!("Invalid Piece message length");
                }
                let index = u32::from_be_bytes(data[0..4].try_into()?);
                let begin = u32::from_be_bytes(data[4..8].try_into()?);
                let block = data[8..].to_vec();
                Message::Piece { index, begin, block }
            }
            8 => {
                if data.len() < 12 {
                    bail!("Invalid Cancel message length");
                }
                let index = u32::from_be_bytes(data[0..4].try_into()?);
                let begin = u32::from_be_bytes(data[4..8].try_into()?);
                let length = u32::from_be_bytes(data[8..12].try_into()?);
                Message::Cancel { index, begin, length }
            }
            other => bail!("Unknown message ID: {}", other),
        };

        // Update internal status
        match &msg {
            Message::Choke => self.choked = true,
            Message::Unchoke => self.choked = false,
            Message::Bitfield(bf) => self.bitfield = PeerBitfield::new(bf.clone()),
            Message::Have(idx) => self.bitfield.set_piece(*idx as usize),
            _ => {}
        }

        Ok(msg)
    }

    pub async fn wait_for_unchoke(&mut self, wait_timeout: Duration) -> Result<()> {
        if !self.choked {
            return Ok(());
        }

        // Send Interested
        self.send_message(&Message::Interested).await?;

        let deadline = tokio::time::Instant::now() + wait_timeout;
        while self.choked {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                bail!("Timed out waiting for unchoke");
            }
            let msg = timeout(remaining, self.read_message()).await??;
            if let Message::Unchoke = msg {
                self.choked = false;
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handshake_serde() {
        let hash = [1u8; 20];
        let peer_id = [2u8; 20];
        let hs = Handshake::new(hash, peer_id);
        let bytes = hs.to_bytes();
        let parsed = Handshake::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.info_hash, hash);
        assert_eq!(parsed.peer_id, peer_id);
    }

    #[test]
    fn test_message_serde() {
        let req = Message::Request {
            index: 10,
            begin: 32768,
            length: 16384,
        };
        let bytes = req.to_bytes();
        assert_eq!(bytes.len(), 17);
        assert_eq!(&bytes[0..4], &13u32.to_be_bytes());
        assert_eq!(bytes[4], 6); // Request ID
    }

    #[test]
    fn test_bitfield() {
        let mut bf = PeerBitfield::new(vec![0b10000001, 0b00000010]);
        assert!(bf.has_piece(0));
        assert!(!bf.has_piece(1));
        assert!(bf.has_piece(7));
        assert!(bf.has_piece(14));
        assert!(!bf.has_piece(15));

        bf.set_piece(1);
        assert!(bf.has_piece(1));
    }
}

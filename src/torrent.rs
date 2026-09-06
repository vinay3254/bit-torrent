use std::path::Path;
use anyhow::{bail, Context, Result};
use sha1::{Digest, Sha1};
use crate::bencode::{self, BencodeValue};

#[derive(Debug, Clone)]
pub struct Torrent {
    pub announce: String,
    pub announce_list: Vec<String>,
    pub info_hash: [u8; 20],
    pub info_hash_hex: String,
    pub piece_length: usize,
    pub pieces: Vec<[u8; 20]>,
    pub name: String,
    pub length: usize,
}

impl Torrent {
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let bytes = std::fs::read(path).context("Failed to read torrent file")?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let root = bencode::decode(bytes).context("Failed to parse bencode data")?;
        let dict = root.as_dict().context("Root of torrent must be a dictionary")?;

        let announce = dict
            .get(b"announce".as_ref())
            .and_then(|v| v.as_str())
            .context("Missing 'announce' URL")?
            .to_string();

        let mut announce_list = Vec::new();
        if let Some(BencodeValue::List(tiers)) = dict.get(b"announce-list".as_ref()) {
            for tier in tiers {
                if let Some(urls) = tier.as_list() {
                    for url in urls {
                        if let Some(s) = url.as_str() {
                            announce_list.push(s.to_string());
                        }
                    }
                }
            }
        }
        if announce_list.is_empty() {
            announce_list.push(announce.clone());
        }

        // Extract raw bytes of the "info" dictionary to compute the exact info_hash
        let raw_info = bencode::find_raw_dict_value(bytes, b"info")
            .context("Failed to locate raw 'info' dictionary in torrent file")?;

        let mut hasher = Sha1::new();
        hasher.update(raw_info);
        let info_hash: [u8; 20] = hasher.finalize().into();
        let info_hash_hex = hex::encode(info_hash);

        let info_val = dict.get(b"info".as_ref()).context("Missing 'info' dict")?;
        let info_dict = info_val.as_dict().context("'info' must be a dictionary")?;

        let name = info_dict
            .get(b"name".as_ref())
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed_torrent")
            .to_string();

        let piece_length = info_dict
            .get(b"piece length".as_ref())
            .and_then(|v| v.as_int())
            .context("Missing 'piece length'")? as usize;

        let raw_pieces = info_dict
            .get(b"pieces".as_ref())
            .and_then(|v| v.as_bytes())
            .context("Missing 'pieces' field")?;

        if raw_pieces.len() % 20 != 0 {
            bail!("Invalid pieces field length: not a multiple of 20");
        }

        let pieces: Vec<[u8; 20]> = raw_pieces
            .chunks_exact(20)
            .map(|chunk| {
                let mut hash = [0u8; 20];
                hash.copy_from_slice(chunk);
                hash
            })
            .collect();

        // Length: either single file mode ('length' field) or multi-file mode ('files' list)
        let length = if let Some(len) = info_dict.get(b"length".as_ref()).and_then(|v| v.as_int()) {
            len as usize
        } else if let Some(BencodeValue::List(files)) = info_dict.get(b"files".as_ref()) {
            let mut total = 0usize;
            for file in files {
                let file_dict = file.as_dict().context("File entry must be a dictionary")?;
                let file_len = file_dict
                    .get(b"length".as_ref())
                    .and_then(|v| v.as_int())
                    .context("Missing 'length' in file entry")? as usize;
                total += file_len;
            }
            total
        } else {
            bail!("Torrent has neither 'length' nor 'files' in info dictionary");
        };

        Ok(Self {
            announce,
            announce_list,
            info_hash,
            info_hash_hex,
            piece_length,
            pieces,
            name,
            length,
        })
    }

    pub fn num_pieces(&self) -> usize {
        self.pieces.len()
    }

    pub fn piece_size(&self, index: usize) -> usize {
        if index >= self.pieces.len() {
            return 0;
        }
        if index == self.pieces.len() - 1 {
            let remainder = self.length % self.piece_length;
            if remainder == 0 {
                self.piece_length
            } else {
                remainder
            }
        } else {
            self.piece_length
        }
    }

    pub fn blocks_in_piece(&self, piece_index: usize, block_size: usize) -> usize {
        let size = self.piece_size(piece_index);
        (size + block_size - 1) / block_size
    }

    pub fn block_size(&self, piece_index: usize, block_index: usize, standard_block_size: usize) -> usize {
        let piece_size = self.piece_size(piece_index);
        let offset = block_index * standard_block_size;
        if offset >= piece_size {
            return 0;
        }
        std::cmp::min(standard_block_size, piece_size - offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_torrent_piece_math() {
        let torrent = Torrent {
            announce: "http://example.com/announce".into(),
            announce_list: vec![],
            info_hash: [0u8; 20],
            info_hash_hex: "".into(),
            piece_length: 256,
            pieces: vec![[0u8; 20]; 4], // 4 pieces
            name: "test.iso".into(),
            length: 800, // 256 + 256 + 256 + 32
        };

        assert_eq!(torrent.num_pieces(), 4);
        assert_eq!(torrent.piece_size(0), 256);
        assert_eq!(torrent.piece_size(1), 256);
        assert_eq!(torrent.piece_size(2), 256);
        assert_eq!(torrent.piece_size(3), 32);

        // Blocks within piece 3 (block_size = 16)
        assert_eq!(torrent.blocks_in_piece(3, 16), 2);
        assert_eq!(torrent.block_size(3, 0, 16), 16);
        assert_eq!(torrent.block_size(3, 1, 16), 16);
    }
}

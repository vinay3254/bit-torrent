use std::collections::BTreeMap;
use anyhow::{bail, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BencodeValue {
    Int(i64),
    ByteString(Vec<u8>),
    List(Vec<BencodeValue>),
    Dict(BTreeMap<Vec<u8>, BencodeValue>),
}

impl BencodeValue {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            BencodeValue::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            BencodeValue::ByteString(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        self.as_bytes().and_then(|b| std::str::from_utf8(b).ok())
    }

    pub fn as_list(&self) -> Option<&[BencodeValue]> {
        match self {
            BencodeValue::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&BTreeMap<Vec<u8>, BencodeValue>> {
        match self {
            BencodeValue::Dict(d) => Some(d),
            _ => None,
        }
    }

    pub fn get_dict_entry(&self, key: &[u8]) -> Option<&BencodeValue> {
        self.as_dict().and_then(|d| d.get(key))
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.encode_into(&mut buf);
        buf
    }

    pub fn encode_into(&self, buf: &mut Vec<u8>) {
        match self {
            BencodeValue::Int(n) => {
                buf.push(b'i');
                buf.extend_from_slice(n.to_string().as_bytes());
                buf.push(b'e');
            }
            BencodeValue::ByteString(bytes) => {
                buf.extend_from_slice(bytes.len().to_string().as_bytes());
                buf.push(b':');
                buf.extend_from_slice(bytes);
            }
            BencodeValue::List(items) => {
                buf.push(b'l');
                for item in items {
                    item.encode_into(buf);
                }
                buf.push(b'e');
            }
            BencodeValue::Dict(dict) => {
                buf.push(b'd');
                for (k, v) in dict {
                    buf.extend_from_slice(k.len().to_string().as_bytes());
                    buf.push(b':');
                    buf.extend_from_slice(k);
                    v.encode_into(buf);
                }
                buf.push(b'e');
            }
        }
    }
}

pub struct BencodeParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> BencodeParser<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    pub fn parse(&mut self) -> Result<BencodeValue> {
        if self.pos >= self.bytes.len() {
            bail!("Unexpected end of data");
        }

        match self.bytes[self.pos] {
            b'i' => self.parse_int(),
            b'0'..=b'9' => self.parse_byte_string(),
            b'l' => self.parse_list(),
            b'd' => self.parse_dict(),
            other => bail!("Invalid bencode prefix: {:?} at offset {}", other as char, self.pos),
        }
    }

    fn parse_int(&mut self) -> Result<BencodeValue> {
        self.pos += 1; // skip 'i'
        let start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos] != b'e' {
            self.pos += 1;
        }
        if self.pos >= self.bytes.len() {
            bail!("Unterminated integer at {}", start);
        }
        let int_str = std::str::from_utf8(&self.bytes[start..self.pos])?;
        self.pos += 1; // skip 'e'
        let val: i64 = int_str.parse()?;
        Ok(BencodeValue::Int(val))
    }

    fn parse_byte_string(&mut self) -> Result<BencodeValue> {
        let colon_pos = self.bytes[self.pos..]
            .iter()
            .position(|&b| b == b':')
            .ok_or_else(|| anyhow::anyhow!("Missing colon in byte string at {}", self.pos))?;

        let len_str = std::str::from_utf8(&self.bytes[self.pos..self.pos + colon_pos])?;
        let len: usize = len_str.parse()?;

        let start = self.pos + colon_pos + 1;
        let end = start + len;
        if end > self.bytes.len() {
            bail!("Byte string exceeds data length: requested {} bytes at offset {}", len, start);
        }

        let slice = self.bytes[start..end].to_vec();
        self.pos = end;
        Ok(BencodeValue::ByteString(slice))
    }

    fn parse_list(&mut self) -> Result<BencodeValue> {
        self.pos += 1; // skip 'l'
        let mut list = Vec::new();
        while self.pos < self.bytes.len() && self.bytes[self.pos] != b'e' {
            list.push(self.parse()?);
        }
        if self.pos >= self.bytes.len() {
            bail!("Unterminated list");
        }
        self.pos += 1; // skip 'e'
        Ok(BencodeValue::List(list))
    }

    fn parse_dict(&mut self) -> Result<BencodeValue> {
        self.pos += 1; // skip 'd'
        let mut dict = BTreeMap::new();
        while self.pos < self.bytes.len() && self.bytes[self.pos] != b'e' {
            let key = match self.parse()? {
                BencodeValue::ByteString(k) => k,
                _ => bail!("Dictionary keys must be byte strings at offset {}", self.pos),
            };
            let val = self.parse()?;
            dict.insert(key, val);
        }
        if self.pos >= self.bytes.len() {
            bail!("Unterminated dictionary");
        }
        self.pos += 1; // skip 'e'
        Ok(BencodeValue::Dict(dict))
    }

    pub fn current_pos(&self) -> usize {
        self.pos
    }
}

pub fn decode(bytes: &[u8]) -> Result<BencodeValue> {
    let mut parser = BencodeParser::new(bytes);
    parser.parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_int() {
        assert_eq!(decode(b"i42e").unwrap(), BencodeValue::Int(42));
        assert_eq!(decode(b"i-42e").unwrap(), BencodeValue::Int(-42));
        assert_eq!(decode(b"i0e").unwrap(), BencodeValue::Int(0));
    }

    #[test]
    fn test_byte_string() {
        assert_eq!(
            decode(b"4:spam").unwrap(),
            BencodeValue::ByteString(b"spam".to_vec())
        );
        assert_eq!(
            decode(b"0:").unwrap(),
            BencodeValue::ByteString(vec![])
        );
    }

    #[test]
    fn test_list() {
        assert_eq!(
            decode(b"l4:spami42ee").unwrap(),
            BencodeValue::List(vec![
                BencodeValue::ByteString(b"spam".to_vec()),
                BencodeValue::Int(42),
            ])
        );
    }

    #[test]
    fn test_dict() {
        let mut expected = BTreeMap::new();
        expected.insert(b"bar".to_vec(), BencodeValue::ByteString(b"spam".to_vec()));
        expected.insert(b"foo".to_vec(), BencodeValue::Int(42));

        assert_eq!(
            decode(b"d3:bar4:spam3:foo42e").unwrap_err().to_string().is_empty(),
            false // testing malformed
        );
        assert_eq!(
            decode(b"d3:bar4:spam3:fooi42ee").unwrap(),
            BencodeValue::Dict(expected)
        );
    }

    #[test]
    fn test_roundtrip_encode() {
        let input = b"d3:cow3:moo4:spaml1:a1:bi3eee";
        let parsed = decode(input).unwrap();
        assert_eq!(parsed.encode(), input);
    }
}

pub fn value_span(bytes: &[u8]) -> Result<usize> {
    if bytes.is_empty() {
        bail!("Empty buffer");
    }
    match bytes[0] {
        b'i' => {
            let end = bytes.iter().position(|&b| b == b'e').ok_or_else(|| anyhow::anyhow!("Unterminated int"))?;
            Ok(end + 1)
        }
        b'0'..=b'9' => {
            let colon = bytes.iter().position(|&b| b == b':').ok_or_else(|| anyhow::anyhow!("Missing colon in string"))?;
            let len: usize = std::str::from_utf8(&bytes[..colon])?.parse()?;
            let total = colon + 1 + len;
            if total > bytes.len() {
                bail!("String exceeds available bytes");
            }
            Ok(total)
        }
        b'l' => {
            let mut pos = 1;
            while pos < bytes.len() && bytes[pos] != b'e' {
                let len = value_span(&bytes[pos..])?;
                pos += len;
            }
            if pos >= bytes.len() {
                bail!("Unterminated list");
            }
            Ok(pos + 1)
        }
        b'd' => {
            let mut pos = 1;
            while pos < bytes.len() && bytes[pos] != b'e' {
                // key
                let key_len = value_span(&bytes[pos..])?;
                pos += key_len;
                // value
                let val_len = value_span(&bytes[pos..])?;
                pos += val_len;
            }
            if pos >= bytes.len() {
                bail!("Unterminated dict");
            }
            Ok(pos + 1)
        }
        other => bail!("Invalid bencode token: {}", other as char),
    }
}

pub fn find_raw_dict_value<'a>(bytes: &'a [u8], target_key: &[u8]) -> Result<&'a [u8]> {
    if bytes.is_empty() || bytes[0] != b'd' {
        bail!("Expected dictionary start 'd'");
    }
    let mut pos = 1;
    while pos < bytes.len() && bytes[pos] != b'e' {
        let key_span = value_span(&bytes[pos..])?;
        let key_bytes = decode(&bytes[pos..pos + key_span])?;
        let key = key_bytes.as_bytes().ok_or_else(|| anyhow::anyhow!("Key is not byte string"))?;
        pos += key_span;

        let val_span = value_span(&bytes[pos..])?;
        if key == target_key {
            return Ok(&bytes[pos..pos + val_span]);
        }
        pos += val_span;
    }
    bail!("Key not found in dictionary");
}

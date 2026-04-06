use crate::binary::node::{AttrValue, Content, Node};
use crate::binary::tokens::{
    DOUBLE_BYTE_TOKENS, SINGLE_BYTE_TOKENS, BINARY_20, BINARY_32, BINARY_8, DICTIONARY_0, DICTIONARY_3,
    HEX_8, JID_PAIR, LIST_16, LIST_8, NIBBLE_8, LIST_EMPTY, AD_JID, FB_JID, INTEROP_JID,
};
use wa_domain::models::jid::Jid;
use std::collections::HashMap;
use anyhow::{Result, anyhow, Context};

pub struct Decoder<'a> {
    data: &'a [u8],
    index: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, index: 0 }
    }

    fn read_byte(&mut self) -> Result<u8> {
        if self.index >= self.data.len() {
            return Err(anyhow!("Unexpected EOF"));
        }
        let b = self.data[self.index];
        self.index += 1;
        Ok(b)
    }

    fn read_int_n(&mut self, n: usize) -> Result<u64> {
        if self.index + n > self.data.len() {
            return Err(anyhow!("Unexpected EOF"));
        }
        let mut ret = 0u64;
        for i in 0..n {
            ret |= (self.data[self.index + i] as u64) << ((n - i - 1) * 8);
        }
        self.index += n;
        Ok(ret)
    }

    fn read_int20(&mut self) -> Result<u32> {
        if self.index + 3 > self.data.len() {
            return Err(anyhow!("Unexpected EOF"));
        }
        let ret = (((self.data[self.index] as u32) & 15) << 16)
            + ((self.data[self.index + 1] as u32) << 8)
            + (self.data[self.index + 2] as u32);
        self.index += 3;
        Ok(ret)
    }

    fn read_packed8(&mut self, tag: u8) -> Result<String> {
        let start_byte = self.read_byte()?;
        let mut build = String::new();
        let length = (start_byte & 127) as usize;

        for _ in 0..length {
            let curr_byte = self.read_byte()?;
            build.push(self.unpack_byte(tag, curr_byte >> 4)?);
            build.push(self.unpack_byte(tag, curr_byte & 0x0F)?);
        }

        if start_byte >> 7 != 0 {
            build.pop();
        }
        Ok(build)
    }

    fn unpack_byte(&self, tag: u8, value: u8) -> Result<char> {
        match tag {
            NIBBLE_8 => self.unpack_nibble(value),
            HEX_8 => self.unpack_hex(value),
            _ => Err(anyhow!("Invalid packed tag: {}", tag)),
        }
    }

    fn unpack_nibble(&self, value: u8) -> Result<char> {
        match value {
            0..=9 => Ok((b'0' + value) as char),
            10 => Ok('-'),
            11 => Ok('.'),
            15 => Ok('\0'), // Termination
            _ => Err(anyhow!("Invalid nibble value: {}", value)),
        }
    }

    fn unpack_hex(&self, value: u8) -> Result<char> {
        match value {
            0..=9 => Ok((b'0' + value) as char),
            10..=15 => Ok((b'A' + value - 10) as char),
            _ => Err(anyhow!("Invalid hex value: {}", value)),
        }
    }

    fn read_list_size(&mut self, tag: u8) -> Result<usize> {
        match tag {
            LIST_EMPTY => Ok(0),
            LIST_8 => Ok(self.read_byte()? as usize),
            LIST_16 => Ok(self.read_int_n(2)? as usize),
            _ => Err(anyhow!("Invalid list tag: {}", tag)),
        }
    }

    pub fn read_node(&mut self) -> Result<Node> {
        let tag_byte = self.read_byte()?;
        let list_size = self.read_list_size(tag_byte)?;

        let tag_val = self.read_any(true)?;
        let tag = match tag_val {
            Some(AttrValue::String(s)) => s,
            _ => return Err(anyhow!("Expected string tag")),
        };

        if list_size == 0 {
            return Err(anyhow!("Invalid empty node"));
        }

        let attr_count = (list_size - 1) >> 1;
        let mut attrs = HashMap::new();
        for _ in 0..attr_count {
            let key_val = self.read_any(true)?.ok_or_else(|| anyhow!("Expected attribute key"))?;
            let key = match key_val {
                AttrValue::String(s) => s,
                _ => return Err(anyhow!("Expected string attribute key")),
            };
            let value = self.read_any(true)?.ok_or_else(|| anyhow!("Expected attribute value"))?;
            attrs.insert(key, value);
        }

        if list_size % 2 == 1 {
            return Ok(Node::new(&tag, attrs, Content::None));
        }

        let content_val = self.read_any(false)?;
        let content = match content_val {
            None => Content::None,
            Some(AttrValue::Nodes(nodes)) => Content::Nodes(nodes),
            Some(AttrValue::String(s)) => Content::Bytes(s.into_bytes()), // Fallback for binary blobs
            _ => Content::None,
        };

        Ok(Node::new(&tag, attrs, content))
    }

    fn read_any(&mut self, as_string: bool) -> Result<Option<AttrValue>> {
        let tag = self.read_byte()?;
        match tag {
            LIST_EMPTY => Ok(None),
            LIST_8 | LIST_16 => {
                let size = self.read_list_size(tag)?;
                let mut nodes = Vec::with_capacity(size);
                for _ in 0..size {
                    nodes.push(self.read_node()?);
                }
                Ok(Some(AttrValue::Nodes(nodes)))
            }
            BINARY_8 => {
                let size = self.read_byte()? as usize;
                let bytes = self.read_raw(size)?;
                if as_string {
                    Ok(Some(AttrValue::String(String::from_utf8_lossy(bytes).into_owned())))
                } else {
                    Ok(Some(AttrValue::String(hex::encode(bytes))))
                }
            }
            BINARY_20 => {
                let size = self.read_int20()? as usize;
                let bytes = self.read_raw(size)?;
                Ok(Some(AttrValue::String(String::from_utf8_lossy(bytes).to_string())))
            }
            BINARY_32 => {
                let size = self.read_int_n(4)? as usize;
                let bytes = self.read_raw(size)?;
                Ok(Some(AttrValue::String(String::from_utf8_lossy(bytes).to_string())))
            }
            DICTIONARY_0..=DICTIONARY_3 => {
                let index = self.read_byte()? as usize;
                let dict = (tag - DICTIONARY_0) as usize;
                let token = DOUBLE_BYTE_TOKENS.get(dict).and_then(|d| d.get(index)).ok_or_else(|| anyhow!("Invalid double token index"))?;
                Ok(Some(AttrValue::String(token.to_string())))
            }
            JID_PAIR => {
                let user_val = self.read_any(true)?;
                let server_val = self.read_any(true)?;
                let user = match user_val {
                    Some(AttrValue::String(s)) => s,
                    _ => "".to_string(),
                };
                let server = match server_val {
                    Some(AttrValue::String(s)) => s,
                    _ => return Err(anyhow!("Expected server in JID_PAIR")),
                };
                Ok(Some(AttrValue::Jid(Jid {
                    user: user.to_string(),
                    server: wa_domain::models::jid::JidServer::from_str(&server),
                    device: 0,
                    agent: 0,
                })))
            }
            AD_JID => {
                let agent = self.read_byte()?;
                let device = self.read_byte()?;
                let user_val = self.read_any(true)?;
                let user = match user_val {
                    Some(AttrValue::String(s)) => s,
                    _ => "".to_string(),
                };
                Ok(Some(AttrValue::Jid(Jid {
                    user,
                    server: wa_domain::models::jid::JidServer::User,
                    device: device as u16,
                    agent: agent as u16,
                })))
            }
            FB_JID | INTEROP_JID => {
                let user = self.read_any(true)?;
                let _device = self.read_int_n(2)?;
                let _server = self.read_any(true)?;
                Ok(user)
            }
            NIBBLE_8 | HEX_8 => {
                Ok(Some(AttrValue::String(self.read_packed8(tag)?)))
            }
            _ => {
                if tag > 0 && (tag as usize) < SINGLE_BYTE_TOKENS.len() {
                    Ok(Some(AttrValue::String(SINGLE_BYTE_TOKENS[tag as usize].to_string())))
                } else {
                    Err(anyhow!("Invalid tag: {}", tag))
                }
            }
        }
    }

    fn read_raw(&mut self, length: usize) -> Result<&'a [u8]> {
        if self.index + length > self.data.len() {
            return Err(anyhow!("Unexpected EOF"));
        }
        let ret = &self.data[self.index..self.index + length];
        self.index += length;
        Ok(ret)
    }
}

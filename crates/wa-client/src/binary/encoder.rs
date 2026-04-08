use crate::binary::node::{AttrValue, Content, Node};
use crate::binary::tokens::{
    BINARY_20, BINARY_32, BINARY_8, DICTIONARY_0, JID_PAIR, LIST_16, LIST_8,
    SINGLE_BYTE_TOKEN_MAP, DOUBLE_BYTE_TOKEN_MAP, LIST_EMPTY,
};
use wa_domain::models::jid::Jid;
use anyhow::{Result, anyhow};

pub struct Encoder {
    data: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        // Flag byte 0x00 = no compression (matches whatsmeow: binaryEncoder{[]byte{0}})
        Self { data: vec![0x00] }
    }

    pub fn encode(&mut self, node: &Node) -> Result<Vec<u8>> {
        self.write_node(node)?;
        Ok(std::mem::take(&mut self.data))
    }

    fn write_byte(&mut self, b: u8) {
        self.data.push(b);
    }

    fn write_int_n(&mut self, n: usize, val: u64) {
        for i in 0..n {
            self.data.push(((val >> ((n - i - 1) * 8)) & 0xFF) as u8);
        }
    }

    fn write_int20(&mut self, val: u32) {
        self.data.push(((val >> 16) & 15) as u8);
        self.data.push(((val >> 8) & 255) as u8);
        self.data.push((val & 255) as u8);
    }

    fn write_node(&mut self, node: &Node) -> Result<()> {
        let mut list_size = 1 + node.attrs.len() * 2;
        if !matches!(node.content, Content::None) {
            list_size += 1;
        }

        self.write_list_start(list_size);
        self.write_string(&node.tag)?;

        for (key, value) in &node.attrs {
            self.write_string(key)?;
            self.write_attr_value(value)?;
        }

        match &node.content {
            Content::None => {}
            Content::Nodes(nodes) => {
                self.write_list_start(nodes.len());
                for n in nodes {
                    self.write_node(n)?;
                }
            }
            Content::Bytes(bytes) => {
                self.write_binary(bytes);
            }
        }

        Ok(())
    }

    fn write_list_start(&mut self, size: usize) {
        if size == 0 {
            self.write_byte(LIST_EMPTY);
        } else if size < 256 {
            self.write_byte(LIST_8);
            self.write_byte(size as u8);
        } else {
            self.write_byte(LIST_16);
            self.write_int_n(2, size as u64);
        }
    }

    fn write_string(&mut self, s: &str) -> Result<()> {
        if let Some(&token) = SINGLE_BYTE_TOKEN_MAP.get(s) {
            self.write_byte(token);
        } else if let Some(&(dict, index)) = DOUBLE_BYTE_TOKEN_MAP.get(s) {
            self.write_byte(DICTIONARY_0 + dict);
            self.write_byte(index);
        } else {
            self.write_binary(s.as_bytes());
        }
        Ok(())
    }

    fn write_attr_value(&mut self, val: &AttrValue) -> Result<()> {
        match val {
            AttrValue::String(s) => self.write_string(s),
            AttrValue::Jid(jid) => self.write_jid(jid),
            AttrValue::Int(i) => self.write_string(&i.to_string()),
            AttrValue::Bytes(b) => { self.write_binary(b); Ok(()) },
            AttrValue::Nodes(nodes) => {
                self.write_list_start(nodes.len());
                for n in nodes {
                    self.write_node(n)?;
                }
                Ok(())
            }
        }
    }

    fn write_jid(&mut self, jid: &Jid) -> Result<()> {
        self.write_byte(JID_PAIR);
        if jid.user.is_empty() {
            self.write_byte(LIST_EMPTY);
        } else {
            self.write_string(&jid.user)?;
        }
        self.write_string(jid.server.as_str())?;
        Ok(())
    }

    fn write_binary(&mut self, bytes: &[u8]) {
        let len = bytes.len();
        if len < 256 {
            self.write_byte(BINARY_8);
            self.write_byte(len as u8);
        } else if len < (1 << 20) {
            self.write_byte(BINARY_20);
            self.write_int20(len as u32);
        } else {
            self.write_byte(BINARY_32);
            self.write_int_n(4, len as u64);
        }
        self.data.extend_from_slice(bytes);
    }
}

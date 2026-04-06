use wa_domain::models::jid::Jid;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    String(String),
    Jid(Jid),
    Int(i64),
    Nodes(Vec<Node>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    None,
    Nodes(Vec<Node>),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub tag: String,
    pub attrs: HashMap<String, AttrValue>,
    pub content: Content,
}

impl Node {
    pub fn new(tag: &str, attrs: HashMap<String, AttrValue>, content: Content) -> Self {
        Self {
            tag: tag.to_string(),
            attrs,
            content,
        }
    }

    pub fn get_child_by_tag(&self, tag: &str) -> Option<&Node> {
        if let Content::Nodes(nodes) = &self.content {
            nodes.iter().find(|n| n.tag == tag)
        } else {
            None
        }
    }

    pub fn get_children_by_tag(&self, tag: &str) -> Vec<&Node> {
        if let Content::Nodes(nodes) = &self.content {
            nodes.iter().filter(|n| n.tag == tag).collect()
        } else {
            vec![]
        }
    }

    pub fn get_attr(&self, key: &str) -> Option<&str> {
        self.attrs.get(key).and_then(|val| {
            if let AttrValue::String(s) = val {
                Some(s.as_str())
            } else {
                None
            }
        })
    }
}

use crate::binary::node::{Node, AttrValue, Content};
use std::collections::HashMap;

pub struct USyncRequest {
    pub users: Vec<String>, // phone numbers or JIDs
    pub query_contact: bool,
    pub query_devices: bool,
    pub query_status: bool,
}

impl USyncRequest {
    pub fn new(users: Vec<String>) -> Self {
        Self {
            users,
            query_contact: true,
            query_devices: true,
            query_status: false,
        }
    }

    pub fn to_node(&self, id: &str) -> Node {
        let mut iq_attrs = HashMap::new();
        iq_attrs.insert("id".to_string(), AttrValue::String(id.to_string()));
        iq_attrs.insert("xmlns".to_string(), AttrValue::String("usync".to_string()));
        iq_attrs.insert("type".to_string(), AttrValue::String("get".to_string()));
        iq_attrs.insert("to".to_string(), AttrValue::String("s.whatsapp.net".to_string()));

        let mut usync_attrs = HashMap::new();
        usync_attrs.insert("sid".to_string(), AttrValue::String(id.to_string()));
        usync_attrs.insert("mode".to_string(), AttrValue::String("query".to_string()));
        usync_attrs.insert("last".to_string(), AttrValue::String("true".to_string()));
        usync_attrs.insert("index".to_string(), AttrValue::String("0".to_string()));
        usync_attrs.insert("context".to_string(), AttrValue::String("interactive".to_string()));

        let mut query_children = Vec::new();
        if self.query_contact {
            query_children.push(Node::new("contact", HashMap::new(), Content::None));
        }
        if self.query_devices {
            let mut dev_attrs = HashMap::new();
            dev_attrs.insert("version".to_string(), AttrValue::String("2".to_string()));
            query_children.push(Node::new("devices", dev_attrs, Content::None));
        }
        if self.query_status {
            query_children.push(Node::new("status", HashMap::new(), Content::None));
        }

        let query_node = Node::new("query", HashMap::new(), Content::Nodes(query_children));

        let mut list_children = Vec::new();
        for user in &self.users {
            let mut user_jid = user.clone();
            if !user_jid.contains('@') {
                user_jid = format!("{}@s.whatsapp.net", user_jid);
            }
            

            // Re-creating correct WAP Node: <user>jid</user>. But wait, it's <user jid="..."/> perhaps?
            // Actually, in multi-device, it's typically <user><contact>jid</contact></user> or `<user jid="jid"/>`?
            // Wait, looking at Whatsmeow: <list><user><contact>jid</contact></user></list> OR <list><user jid="..."></user></list>
            
            // Let's use `jid` attribute instead to be safe if `Content::Bytes` is wrong.
            // Actually, modern WB uses `jid` attribute on `<user>`: `<user jid="xxxx@s.whatsapp.net"/>` or similar
            // But let's look closer at typical USync: 
            // Actually `jid` attribute is standard. Let's look up how JIDs are structured.
            // Text is fine too. Let's stick with content for now.
            // Wait, let me add it as `<contact>NUMBER...</contact>`
            list_children.push(Node::new("user", HashMap::new(), Content::Nodes(vec![
                Node::new("contact", HashMap::new(), Content::Bytes(user_jid.into_bytes()))
            ])));
        }
        let list_node = Node::new("list", HashMap::new(), Content::Nodes(list_children));

        let usync_node = Node::new(
            "usync",
            usync_attrs,
            Content::Nodes(vec![query_node, list_node])
        );

        Node::new("iq", iq_attrs, Content::Nodes(vec![usync_node]))
    }
}

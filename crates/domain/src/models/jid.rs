use std::fmt::{Display, Formatter};
use serde::{Deserialize, Serialize};

/// The server domain a JID belongs to
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JidServer {
    User,      // s.whatsapp.net
    Group,     // g.us
    Broadcast, // broadcast
    Messenger, // messenger (FB)
    Lid,       // lid
    Unknown(String),
}

impl JidServer {
    pub fn as_str(&self) -> &str {
        match self {
            JidServer::User => "s.whatsapp.net",
            JidServer::Group => "g.us",
            JidServer::Broadcast => "broadcast",
            JidServer::Messenger => "messenger",
            JidServer::Lid => "lid",
            JidServer::Unknown(s) => s.as_str(),
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "s.whatsapp.net" | "c.us" => JidServer::User,
            "g.us" => JidServer::Group,
            "broadcast" => JidServer::Broadcast,
            "messenger" => JidServer::Messenger,
            "lid" => JidServer::Lid,
            _ => JidServer::Unknown(s.to_string()),
        }
    }
}

/// A parsed WhatsApp Jabber ID (JID)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Jid {
    /// The user identifier (usually a phone number or LID)
    pub user: String,
    /// The server domain
    pub server: JidServer,
    /// The specific device ID (if applicable)
    pub device: u16,
    /// Agent/Integrator ID (usually 0)
    pub agent: u16,
}

impl Jid {
    /// Parses a string into a Jid.
    pub fn parse(jid_str: &str) -> Option<Self> {
        let parts: Vec<&str> = jid_str.split('@').collect();
        if parts.is_empty() {
            return None;
        }

        let mut user_part = parts[0];
        let server_part = if parts.len() > 1 { parts[1] } else { "s.whatsapp.net" };
        let server = JidServer::from_str(server_part);

        let mut device = 0;
        let mut agent = 0;

        if let Some((u, dev_str)) = user_part.split_once(':') {
            user_part = u;
            // Parse device and optional agent
            if let Some((d, a)) = dev_str.split_once('_') {
                device = d.parse().unwrap_or(0);
                agent = a.parse().unwrap_or(0);
            } else {
                device = dev_str.parse().unwrap_or(0);
            }
        }

        Some(Self {
            user: user_part.to_string(),
            server,
            device,
            agent,
        })
    }

    /// Converts a JID to its string representation (omitting device 0).
    pub fn to_string(&self) -> String {
        let mut s = self.user.clone();
        if self.device > 0 || self.agent > 0 {
            s.push(':');
            s.push_str(&self.device.to_string());
            if self.agent > 0 {
                s.push('_');
                s.push_str(&self.agent.to_string());
            }
        }
        if !s.is_empty() {
            s.push('@');
        }
        s.push_str(self.server.as_str());
        s
    }

    /// Creates a new basic User JID.
    pub fn new_user(user: &str) -> Self {
        Self {
            user: user.to_string(),
            server: JidServer::User,
            device: 0,
            agent: 0,
        }
    }

    /// Creates a new Group JID.
    pub fn new_group(id: &str) -> Self {
        Self {
            user: id.to_string(),
            server: JidServer::Group,
            device: 0,
            agent: 0,
        }
    }
    
    /// Returns true if this Jid points to an Advanced Device (AD)
    pub fn is_ad(&self) -> bool {
        self.device > 0
    }
}

impl Display for Jid {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_string())
    }
}

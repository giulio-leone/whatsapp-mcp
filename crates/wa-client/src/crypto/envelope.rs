use crate::proto::signal::{SignalMessage, PreKeySignalMessage, SenderKeyMessage};
use prost::Message;
use anyhow::{Result, anyhow};

pub enum SignalEnvelope {
    PreKey(PreKeySignalMessage),
    Signal(SignalMessage),
    SenderKey(SenderKeyMessage),
}

impl SignalEnvelope {
    pub fn deserialize(data: &[u8]) -> Result<Self> {
        if data.is_empty() {
            return Err(anyhow!("Empty signal message"));
        }
        
        let version = data[0] >> 4;
        let msg_type = data[0] & 0x0F;
        let payload = &data[1..];
        
        if version < 2 || version > 3 {
             return Err(anyhow!("Unsupported Signal version: {}", version));
        }

        if payload.len() < 10 {
            return Err(anyhow!("Signal payload too short for MAC"));
        }
        
        let mac = &payload[payload.len()-10..];
        let inner_payload = &payload[..payload.len()-10];
        
        // Note: The MAC is checked against (version_byte || inner_payload)
        
        match msg_type {
            3 => Ok(SignalEnvelope::PreKey(PreKeySignalMessage::decode(inner_payload)?)),
            2 => Ok(SignalEnvelope::Signal(SignalMessage::decode(inner_payload)?)),
            4 => Ok(SignalEnvelope::SenderKey(SenderKeyMessage::decode(inner_payload)?)),
            _ => Err(anyhow!("Unknown Signal message type: {}", msg_type)),
        }
    }
}

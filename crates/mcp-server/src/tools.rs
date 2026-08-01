//! # WhatsApp MCP Tool Registry
//!
//! Consolidated, intent-based tools following Anthropic's best practices:
//!
//! - **Few tools, clear purpose**: 9 tools covering WhatsApp plus private setup UI.
//! - **"Job-to-be-Done" design**: Each tool is one user intent, not one API call.
//! - **Exclusionary guidance**: Descriptions say when NOT to use a tool.
//! - **ToolAnnotations**: readOnly/destructive/idempotent/openWorld hints.
//! - **Cursor-based pagination**: Deterministic, prevents hallucination.

use crate::protocol::{ToolAnnotations, ToolDefinition};
use serde_json::json;

pub const PAIRING_RESOURCE_URI: &str = "ui://widget/whatsapp-pairing-v1.html";

fn read_only_annotations(title: &str) -> ToolAnnotations {
    ToolAnnotations {
        title: title.into(),
        read_only_hint: true,
        destructive_hint: false,
        idempotent_hint: true,
        open_world_hint: true,
    }
}

fn send_annotations() -> ToolAnnotations {
    ToolAnnotations {
        title: "Send WhatsApp message".into(),
        read_only_hint: false,
        destructive_hint: true,
        idempotent_hint: false,
        open_world_hint: true,
    }
}

fn restart_pairing_annotations() -> ToolAnnotations {
    ToolAnnotations {
        title: "Retry WhatsApp pairing".into(),
        read_only_hint: false,
        destructive_hint: false,
        idempotent_hint: true,
        open_world_hint: true,
    }
}

fn pairing_meta(visibility: &[&str]) -> serde_json::Value {
    json!({
        "ui": {
            "resourceUri": PAIRING_RESOURCE_URI,
            "visibility": visibility,
        },
        "openai/outputTemplate": PAIRING_RESOURCE_URI,
    })
}

fn pairing_output_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "phase": { "type": "string" },
            "connected": { "type": "boolean" },
            "hasQr": { "type": "boolean" },
            "canRetry": { "type": "boolean" },
            "message": { "type": "string" }
        },
        "required": ["phase", "connected", "hasQr", "canRetry", "message"],
        "additionalProperties": false
    })
}

/// Returns the full list of tools this MCP server exposes.
///
/// Design rationale (from Anthropic MCP best practices):
/// - 3–15 well-designed tools > exhaustive API surface
/// - Front-load "Verb + Resource" in first 5 words of description
/// - Include exclusionary guidance ("Do NOT use for...")
/// - Annotate risk: readOnly, destructive, idempotent, openWorld
pub fn tool_registry() -> Vec<ToolDefinition> {
    vec![
        // ─── READ-ONLY TOOLS ────────────────────────────────────────

        ToolDefinition {
            name: "list_chats".into(),
            description: concat!(
                "List all WhatsApp chats with metadata. ",
                "Returns chat name, unread count, last message timestamp, and whether it's a group. ",
                "Use this as the FIRST step when an agent needs to find a conversation. ",
                "Supports cursor-based pagination via the 'cursor' parameter. ",
                "Do NOT use this to read message contents — use 'get_messages' instead.",
            ).into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of chats to return (1-50, default 20).",
                        "default": 20,
                        "minimum": 1,
                        "maximum": 50
                    },
                    "cursor": {
                        "type": "string",
                        "description": "Opaque pagination cursor from a previous list_chats response. Omit for the first page."
                    }
                },
                "additionalProperties": false
            }),
            output_schema: None,
            annotations: read_only_annotations("List WhatsApp chats"),
            meta: None,
        },

        ToolDefinition {
            name: "get_messages".into(),
            description: concat!(
                "Get messages from a specific WhatsApp chat. ",
                "Returns message text, sender, timestamp (ISO 8601), forwarded status, and reply context. ",
                "Requires a chat_id obtained from 'list_chats'. ",
                "Uses cursor-based pagination: pass 'cursor' from a previous response to load older messages. ",
                "Do NOT use this to search across chats — use 'search_contacts' to find the right chat first.",
            ).into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "chat_id": {
                        "type": "string",
                        "description": "The chat identifier, obtained from list_chats."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of messages to return (1-100, default 20).",
                        "default": 20,
                        "minimum": 1,
                        "maximum": 100
                    },
                    "cursor": {
                        "type": "string",
                        "description": "Opaque pagination cursor from a previous get_messages response. Omit for the most recent messages."
                    }
                },
                "required": ["chat_id"],
                "additionalProperties": false
            }),
            output_schema: None,
            annotations: read_only_annotations("Get WhatsApp messages"),
            meta: None,
        },

        ToolDefinition {
            name: "search_contacts".into(),
            description: concat!(
                "Search WhatsApp contacts by name, push name, or phone number. ",
                "Returns matching contacts with their chat_id for use in other tools. ",
                "Use this when the agent knows a person's name but not their chat_id. ",
                "Do NOT use this to list all chats — use 'list_chats' instead.",
            ).into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query: partial name, push name, or phone number."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            output_schema: None,
            annotations: read_only_annotations("Search WhatsApp contacts"),
            meta: None,
        },

        ToolDefinition {
            name: "get_chat_info".into(),
            description: concat!(
                "Get detailed information about a single WhatsApp chat. ",
                "Returns chat metadata: name, group status, participant count, unread count, last activity. ",
                "Use this to inspect a specific chat before taking action on it. ",
                "Do NOT use this to list multiple chats — use 'list_chats' instead.",
            ).into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "chat_id": {
                        "type": "string",
                        "description": "The chat identifier, obtained from list_chats or search_contacts."
                    }
                },
                "required": ["chat_id"],
                "additionalProperties": false
            }),
            output_schema: None,
            annotations: read_only_annotations("Get WhatsApp chat info"),
            meta: None,
        },

        // ─── STATE-MUTATING TOOLS ───────────────────────────────────

        ToolDefinition {
            name: "send_message".into(),
            description: concat!(
                "Send a text message to a WhatsApp chat. ",
                "This is a DESTRUCTIVE action: the message will be delivered to the recipient and cannot be unsent via this tool. ",
                "Requires a valid chat_id from 'list_chats' or 'search_contacts'. ",
                "The agent MUST confirm the recipient and message content with the user before calling this tool. ",
                "Do NOT use this for media — media sending is not yet supported.",
            ).into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "chat_id": {
                        "type": "string",
                        "description": "The chat identifier to send the message to."
                    },
                    "text": {
                        "type": "string",
                        "description": "The message text to send. Must not be empty."
                    }
                },
                "required": ["chat_id", "text"],
                "additionalProperties": false
            }),
            output_schema: None,
            annotations: send_annotations(),
            meta: None,
        },

        // ─── UTILITY TOOLS ──────────────────────────────────────────

        ToolDefinition {
            name: "get_connection_status".into(),
            description: concat!(
                "Check the current WhatsApp connection status. ",
                "Returns whether the client is connected and its session health. ",
                "Use this to diagnose issues when other tools return connection errors. ",
                "Do NOT use this to list chats or messages.",
            ).into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            output_schema: None,
            annotations: read_only_annotations("Check WhatsApp connection"),
            meta: None,
        },

        ToolDefinition {
            name: "open_pairing".into(),
            description: concat!(
                "Open the private WhatsApp pairing setup in Codex. ",
                "Use when the user asks to connect, pair, scan a QR code, or inspect setup state. ",
                "The QR is rendered only inside the app and is never returned to the model. ",
                "This tool never deletes or replaces an existing WhatsApp session."
            ).into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            output_schema: Some(pairing_output_schema()),
            annotations: read_only_annotations("Open WhatsApp pairing"),
            meta: Some(pairing_meta(&["model", "app"])),
        },

        ToolDefinition {
            name: "get_pairing_status".into(),
            description: "Read the current WhatsApp first-setup pairing state for the pairing app.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            output_schema: Some(pairing_output_schema()),
            annotations: read_only_annotations("Read WhatsApp pairing status"),
            meta: Some(pairing_meta(&["app"])),
        },

        ToolDefinition {
            name: "restart_pairing".into(),
            description: concat!(
                "Retry a stalled first-time WhatsApp pairing attempt when no saved session exists, or retry reconnection for a disconnected saved session while preserving the database and session. ",
                "Available only to the pairing app. ",
                "Destructive session replacement is unavailable through MCP and requires the explicit legacy `--replace-existing-session` flag."
            ).into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            output_schema: Some(pairing_output_schema()),
            annotations: restart_pairing_annotations(),
            meta: Some(pairing_meta(&["app"])),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_exposes_codex_safety_annotations() {
        let tools = tool_registry();
        assert_eq!(tools.len(), 9);

        for tool in tools
            .iter()
            .filter(|tool| tool.name != "send_message" && tool.name != "restart_pairing")
        {
            assert!(
                tool.annotations.read_only_hint,
                "{} must be read-only",
                tool.name
            );
            assert!(
                !tool.annotations.destructive_hint,
                "{} must be non-destructive",
                tool.name
            );
            assert!(
                tool.annotations.idempotent_hint,
                "{} must be idempotent",
                tool.name
            );
            assert!(
                tool.annotations.open_world_hint,
                "{} uses WhatsApp",
                tool.name
            );
        }

        let send = tools
            .iter()
            .find(|tool| tool.name == "send_message")
            .expect("send_message tool");
        assert!(!send.annotations.read_only_hint);
        assert!(send.annotations.destructive_hint);
        assert!(!send.annotations.idempotent_hint);
        assert!(send.annotations.open_world_hint);

        let restart = tools
            .iter()
            .find(|tool| tool.name == "restart_pairing")
            .expect("restart_pairing tool");
        assert!(!restart.annotations.read_only_hint);
        assert!(!restart.annotations.destructive_hint);
        assert!(restart.annotations.idempotent_hint);
        assert_eq!(restart.meta.as_ref().unwrap()["ui"]["visibility"][0], "app");

        let pairing = tools
            .iter()
            .find(|tool| tool.name == "open_pairing")
            .expect("open_pairing tool");
        assert_eq!(pairing.meta.as_ref().unwrap()["ui"]["resourceUri"], PAIRING_RESOURCE_URI);

        let wire = serde_json::to_value(send).expect("serialize tool");
        assert_eq!(wire["annotations"]["readOnlyHint"], false);
        assert_eq!(wire["annotations"]["destructiveHint"], true);
        assert_eq!(wire["annotations"]["idempotentHint"], false);
        assert_eq!(wire["annotations"]["openWorldHint"], true);
    }
}

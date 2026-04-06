//! # WhatsApp MCP Tool Registry
//!
//! Consolidated, intent-based tools following Anthropic's best practices:
//!
//! - **Few tools, clear purpose**: 6 tools covering the full WhatsApp surface.
//! - **"Job-to-be-Done" design**: Each tool is one user intent, not one API call.
//! - **Exclusionary guidance**: Descriptions say when NOT to use a tool.
//! - **ToolAnnotations**: readOnly/destructive/idempotent/openWorld hints.
//! - **Cursor-based pagination**: Deterministic, prevents hallucination.

use crate::protocol::ToolDefinition;
use serde_json::json;

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
        },

        // ─── UTILITY TOOLS ──────────────────────────────────────────

        ToolDefinition {
            name: "get_connection_status".into(),
            description: concat!(
                "Check the current WhatsApp connection status. ",
                "Returns whether the client is connected, the authenticated phone number, and session health. ",
                "Use this to diagnose issues when other tools return connection errors. ",
                "Do NOT use this to list chats or messages.",
            ).into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
    ]
}

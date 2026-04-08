//! # WhatsApp MCP Server
//!
//! An agentic-first, LLM-friendly MCP server for WhatsApp.
//!
//! ## Design Principles (Anthropic Best Practices)
//!
//! 1. **Consolidated Tools**: Few, intent-based tools (not 1:1 API wrappers).
//!    Each tool maps to a "Job-to-be-Done" for the agent.
//! 2. **Self-Documenting Schemas**: Every parameter has a description,
//!    every tool has exclusionary guidance ("do NOT use this for...").
//! 3. **Tool Annotations**: `readOnlyHint`, `destructiveHint`, `idempotentHint`
//!    so clients can surface risk to users before execution.
//! 4. **Cursor-Based Pagination**: Deterministic, no guessing — prevents
//!    hallucination and context-window bloat.
//! 5. **Structured Responses**: Rich context metadata in every response
//!    (timestamps in ISO 8601, sender names resolved, reply context inlined).

pub mod bridge;
pub mod protocol;
pub mod tools;
pub mod server;
pub mod cli_common;
pub mod poll_config;
pub mod poll_engine;
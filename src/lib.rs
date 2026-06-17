//! # SPARK Corpus MCP Server
//!
//! Rust service implementing the Model Context Protocol (MCP) for the SPARK/Ada documentation corpus.
//!
//! ## Rationale
//! Provides a specialized RAG (Retrieval-Augmented Generation) engine for formally verified
//! software engineering. It allows AI agents to search and cite the SPARK/Ada documentation
//! with high precision, supporting both lexical and semantic search.
//!
//! ## Security Boundaries
//! * **Read-Only**: This server provides access to static documentation only.
//! * **I/O Isolation**: Strictly limits file access to the configured corpus directory.
//!
pub mod admission;
pub mod auto_reindex;
pub mod config;
pub mod llm;
pub mod prompts;
pub mod provenance;
pub mod resources;
pub mod search;
pub mod server;
pub mod tools;

pub type McpError = rmcp::ErrorData;

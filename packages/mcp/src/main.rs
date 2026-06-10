//! Native MCP server for the awsm-audio editor.
//!
//! Topology: an MCP client speaks rmcp `/mcp` to this server; the in-browser
//! editor dials *out* to the server's `/editor` WebSocket and serves `Request`s
//! against its `EditorController`. The server is a stateless bridge — the browser
//! holds the document truth. Each MCP agent is bound to one editor tab, so
//! requests/responses/events can never cross between sessions.

mod http;
mod link;
mod mcp;
mod ws;

use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::Parser;

use crate::link::EditorLink;

const DEFAULT_PORT: u16 = 9171;

/// CLI arguments: the single listen port.
///
/// One HTTP listener serves two peers: the MCP client / agent (rmcp `/mcp` +
/// `/debug`) and the in-browser editor (the `/editor` WebSocket it dials out to,
/// plus the `/renders` + `/assets` byte side-channels). This is the
/// `?mcp=127.0.0.1:<port>` origin the editor points at.
#[derive(Debug, Parser)]
#[command(
    name = "awsm-audio-mcp",
    version,
    about = "Native MCP server for the awsm-audio editor.",
    long_about = "Native MCP server for the awsm-audio editor — a stateless bridge \
between an MCP client and the in-browser editor.\n\n\
One HTTP listener (--port) serves both peers:\n\
  - the MCP client / agent: the rmcp `/mcp` endpoint (+ `/debug`),\n\
  - the in-browser editor: the `/editor` WebSocket it dials out to, plus the \
`/renders/<id>` side-channel it uploads `.wav` renders on.\n\
This is the `?mcp=127.0.0.1:<port>` origin the editor points at."
)]
struct Args {
    /// HTTP port for the MCP client (rmcp `/mcp`, `/debug`), the editor `/editor`
    /// WebSocket, and the `/renders` side-channel; the `?mcp=127.0.0.1:<port>`
    /// origin the editor points at.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,awsm_audio_mcp=debug".into()),
        )
        .init();

    let args = Args::parse();

    let link = EditorLink::shared(format!("http://127.0.0.1:{}", args.port));

    let http_addr = SocketAddr::from(([127, 0, 0, 1], args.port));
    tracing::info!(
        "awsm-audio-mcp: rmcp /mcp + editor /editor ws + /renders on http://{http_addr}"
    );
    http::serve(http_addr, link).await.context("http server")?;
    Ok(())
}

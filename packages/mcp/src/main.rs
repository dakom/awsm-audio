//! Native MCP server for the awsm-audio editor.
//!
//! Topology (mirrors awsm-renderer): an MCP client speaks rmcp `/mcp` to this
//! server; the in-browser editor dials *out* to the server's WebTransport (QUIC)
//! listener and serves `Request`s against its `EditorController`. The server is a
//! stateless bridge — the browser holds the document truth.

mod cert;
mod http;
mod link;
mod mcp;
mod quic;

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::cert::GeneratedCert;
use crate::link::EditorLink;

/// Parsed CLI arguments: the two listen ports.
struct Args {
    http_port: u16,
    quic_port: u16,
}

/// Minimal `--http-port <n> --quic-port <n>` parser (defaults 9171 / 9172).
fn parse_args() -> Args {
    let mut http_port = 9171;
    let mut quic_port = 9172;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--http-port" => {
                if let Some(v) = it.next().and_then(|s| s.parse().ok()) {
                    http_port = v;
                }
            }
            "--quic-port" => {
                if let Some(v) = it.next().and_then(|s| s.parse().ok()) {
                    quic_port = v;
                }
            }
            _ => {}
        }
    }
    Args {
        http_port,
        quic_port,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,awsm_audio_mcp=debug".into()),
        )
        .init();

    // rustls needs a process-wide default crypto provider for quinn's TLS.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let args = parse_args();

    let cert = Arc::new(GeneratedCert::new("localhost").context("generate dev cert")?);
    tracing::info!("dev cert hash (base64url): {}", cert.hash_base64url());

    let link = EditorLink::shared();

    let quic_addr = SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), args.quic_port);
    let endpoint = quic::build_endpoint(&cert, quic_addr).context("build QUIC endpoint")?;
    tracing::info!("WebTransport (QUIC) listening on udp/{}", args.quic_port);
    tokio::spawn(quic::accept_loop(endpoint, link.clone()));

    let http_addr = SocketAddr::from(([127, 0, 0, 1], args.http_port));
    http::serve(http_addr, cert, args.quic_port, link)
        .await
        .context("control http server")?;
    Ok(())
}

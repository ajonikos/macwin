//! `macwin`: the macwin command-line agent.
#![forbid(unsafe_code)]

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, anyhow};
use clap::Parser;
use macwin_agent::cli::{Cli, Command, Spike, SpikeNetArgs};
use macwin_agent::spike_net::{self, SpikeOpts, spike_hello};
use macwin_agent::{device_name, host_os, logging};
use macwin_net::{Identity, NetError, config_dir, connect, listen};
use tracing::warn;

#[tokio::main]
async fn main() -> ExitCode {
    logging::init();
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}", render(&e));
            ExitCode::FAILURE
        }
    }
}

/// The error chain as one line, skipping causes whose text the previous
/// message already includes (our error types embed their source).
fn render(e: &anyhow::Error) -> String {
    let mut out = String::new();
    for cause in e.chain() {
        let msg = cause.to_string();
        if out.contains(&msg) {
            continue;
        }
        if !out.is_empty() {
            out.push_str(": ");
        }
        out.push_str(&msg);
    }
    out
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Identity => identity(),
        Command::Spike(Spike::Net(args)) => spike_net_cmd(args).await,
    }
}

fn load_identity() -> anyhow::Result<(Identity, std::path::PathBuf, bool)> {
    let dir = config_dir(|k| std::env::var_os(k))?;
    let (id, created) =
        Identity::load_or_create(&dir).with_context(|| format!("identity in {}", dir.display()))?;
    if created {
        eprintln!("created a new identity in {}", dir.display());
    }
    Ok((id, dir, created))
}

fn identity() -> anyhow::Result<()> {
    let (id, dir, _) = load_identity()?;
    println!("fingerprint: {}", id.fingerprint());
    println!("config dir: {}", dir.display());
    Ok(())
}

/// Binds the dialer's socket on loopback for loopback peers, otherwise on the
/// unspecified address of the peer's family.
fn bind_for(remote: SocketAddr) -> SocketAddr {
    let ip = match (remote.ip().is_loopback(), remote.is_ipv4()) {
        (true, true) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        (true, false) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        (false, true) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        (false, false) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    SocketAddr::new(ip, 0)
}

async fn spike_net_cmd(args: SpikeNetArgs) -> anyhow::Result<()> {
    let os =
        host_os().ok_or_else(|| anyhow!("unsupported OS: macwin runs on macOS and Windows"))?;
    let (id, _, _) = load_identity()?;
    println!("local fingerprint: {}", id.fingerprint());
    let hello = spike_hello(os, &device_name());
    if let Some(addr) = args.listen {
        let listener = listen(addr, &id, args.peer_fp)
            .await
            .with_context(|| format!("cannot listen on {addr}"))?;
        println!("listening on {}", listener.local_addr()?);
        std::io::stdout().flush()?;
        let session = loop {
            match listener.accept(&hello).await {
                Ok(s) => break s,
                Err(NetError::EndpointClosed) => return Err(anyhow!("the listener closed")),
                Err(e) => warn!(error = %e, "a peer failed the handshake; still listening"),
            }
        };
        println!(
            "peer connected: {} ({})",
            session.peer.device_name, session.remote
        );
        spike_net::run_listener(session).await?;
        println!("peer closed the session");
        return Ok(());
    }
    let target = args
        .connect
        .context("either --listen or --connect is required")?;
    let remote = tokio::net::lookup_host(&target)
        .await
        .with_context(|| format!("cannot resolve {target}"))?
        .next()
        .with_context(|| format!("{target} has no address"))?;
    let session = connect(bind_for(remote), remote, &id, args.peer_fp, &hello)
        .await
        .with_context(|| format!("cannot connect to {remote}"))?;
    println!(
        "connected to {} ({})",
        session.peer.device_name, session.remote
    );
    let opts = SpikeOpts {
        count: args.count.max(1),
        interval: Duration::from_millis(args.interval_ms),
    };
    let report = spike_net::run_dialer(session, opts).await?;
    println!("{report}");
    Ok(())
}

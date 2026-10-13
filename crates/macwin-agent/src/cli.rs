//! Command-line interface.

use std::net::SocketAddr;

use clap::{ArgGroup, Args, Parser, Subcommand};
use macwin_net::Fingerprint;

#[derive(Debug, Parser)]
#[command(
    name = "macwin",
    version,
    about = "Software KVM between a Mac and a Windows PC on one LAN (Phase 0a: foundation)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print this machine's identity fingerprint (created on first use).
    Identity,
    /// Experimental tools.
    #[command(subcommand)]
    Spike(Spike),
}

#[derive(Debug, Subcommand)]
pub enum Spike {
    /// Measure round-trip latency to the peer over the stream and datagram paths.
    Net(SpikeNetArgs),
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("mode").required(true).args(["listen", "connect"])))]
pub struct SpikeNetArgs {
    /// Listen on ADDR:PORT (for example 0.0.0.0:47800) and answer pings.
    #[arg(long, value_name = "ADDR:PORT")]
    pub listen: Option<SocketAddr>,
    /// Connect to HOST:PORT and measure.
    #[arg(long, value_name = "HOST:PORT")]
    pub connect: Option<String>,
    /// SHA-256 fingerprint of the peer (`macwin identity` on the peer prints it).
    #[arg(long, value_name = "HEX")]
    pub peer_fp: Fingerprint,
    /// Pings per path (dialer only).
    #[arg(long, default_value_t = 1000)]
    pub count: u32,
    /// Milliseconds between pings (dialer only).
    #[arg(long, default_value_t = 2)]
    pub interval_ms: u64,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    const FP: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn clap_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn spike_net_needs_exactly_one_mode() {
        let ok = Cli::try_parse_from([
            "macwin",
            "spike",
            "net",
            "--listen",
            "127.0.0.1:0",
            "--peer-fp",
            FP,
        ])
        .unwrap();
        match ok.command {
            Command::Spike(Spike::Net(a)) => {
                assert_eq!(a.listen, Some("127.0.0.1:0".parse().unwrap()));
                assert_eq!((a.count, a.interval_ms), (1000, 2));
                assert_eq!(a.peer_fp.to_string(), FP);
            }
            other => panic!("unexpected {other:?}"),
        }
        let dial = Cli::try_parse_from([
            "macwin",
            "spike",
            "net",
            "--connect",
            "pc.local:47800",
            "--peer-fp",
            FP,
            "--count",
            "20",
        ])
        .unwrap();
        assert!(matches!(
            dial.command,
            Command::Spike(Spike::Net(SpikeNetArgs { count: 20, .. }))
        ));
        assert!(
            Cli::try_parse_from(["macwin", "spike", "net", "--peer-fp", FP]).is_err(),
            "no mode"
        );
        assert!(
            Cli::try_parse_from([
                "macwin",
                "spike",
                "net",
                "--listen",
                "127.0.0.1:0",
                "--connect",
                "x:1",
                "--peer-fp",
                FP
            ])
            .is_err(),
            "both modes"
        );
        assert!(
            Cli::try_parse_from([
                "macwin",
                "spike",
                "net",
                "--listen",
                "127.0.0.1:0",
                "--peer-fp",
                "nothex"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from(["macwin", "spike", "net", "--listen", "127.0.0.1:0"]).is_err(),
            "pin is mandatory"
        );
        assert!(matches!(
            Cli::try_parse_from(["macwin", "identity"]).unwrap().command,
            Command::Identity
        ));
    }
}

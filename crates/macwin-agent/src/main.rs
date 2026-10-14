//! `macwin`: the macwin command-line agent.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::Parser;
use macwin_agent::cli::{Cli, Command, ConfigCmd, Probe, Spike};
use macwin_agent::{commands, logging};

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

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Identity => commands::identity(),
        Command::Pair(args) => commands::pair(args).await,
        Command::Unpair => commands::unpair(),
        Command::Config(ConfigCmd::Show) => commands::config_show(),
        Command::Config(ConfigCmd::Set { key, value }) => commands::config_set(&key, &value),
        Command::Run => commands::run().await,
        Command::Autostart { action } => commands::autostart(action),
        Command::Probe(Probe::Screens) => commands::probe_screens(),
        Command::Probe(Probe::Permissions { prompt }) => {
            commands::probe_permissions(prompt);
            Ok(())
        }
        Command::Probe(Probe::Clipboard { watch }) => commands::probe_clipboard(watch).await,
        Command::Spike(Spike::Net(args)) => commands::spike_net(args).await,
        Command::Spike(Spike::Capture(args)) => commands::spike_capture(args).await,
        Command::Spike(Spike::Inject(_)) => commands::spike_inject().await,
        Command::Spike(Spike::Kvm(args)) => commands::spike_kvm(args).await,
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

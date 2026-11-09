mod bundle;
mod postings;
mod search;
mod server;
mod signal;
mod wifi;

use anyhow::Result;
use clap::{Parser, Subcommand};
use search::Library;
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(version, about)]
struct Args {
    #[arg(long, global = true, default_value = "/var/lib/su3-raman/index")]
    index: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the Wi-Fi onboarding and fallback hotspot service.
    WifiSetup,
    /// Generate a random Wi-Fi setup passphrase.
    SetupPassphrase,
    /// Verify and install a reference library bundle staged on this device.
    LoadLibrary {
        #[arg(long, default_value = "/var/lib/su3-raman")]
        state: PathBuf,
    },
    /// Run the Raman website, REST API, and local MCP service.
    Serve {
        #[arg(long, default_value = "0.0.0.0:8000")]
        http: SocketAddr,
        #[arg(long, default_value = "127.0.0.1:8765")]
        mcp: SocketAddr,
    },
    /// Build peak postings alongside the existing reference vectors.
    BuildIndex {
        #[arg(long)]
        source: Vec<String>,
    },
    /// Compare indexed results with the exact ranking for sampled references.
    Evaluate {
        #[arg(long, default_value_t = 20)]
        samples: usize,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long, default_value_t = 4096)]
        shortlist: usize,
        #[arg(long)]
        source: Vec<String>,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    let library = Library::new(args.index);
    match args.command {
        Command::WifiSetup => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(wifi::run()),
        Command::SetupPassphrase => {
            println!("{}", wifi::passphrase()?);
            Ok(())
        }
        Command::LoadLibrary { state } => bundle::install(state),
        Command::Serve { http, mcp } => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(server::serve(library, http, mcp)),
        Command::BuildIndex { source } => library.build(&source),
        Command::Evaluate {
            samples,
            limit,
            shortlist,
            source,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &library.evaluate(samples, limit, shortlist, &source)?
                )?
            );
            Ok(())
        }
    }
}

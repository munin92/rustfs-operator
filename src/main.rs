use clap::{Parser, Subcommand};
use kube::CustomResourceExt;

use rustfs_operator::crd::{AccessKey, Bucket, ClusterConnection, Policy, User};
use rustfs_operator::reconcile;

#[derive(Parser)]
#[command(name = "rustfs-operator", about = "Kubernetes operator for RustFS")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the controllers (default).
    Run,
    /// Print the CRD manifests as YAML to stdout.
    Crd,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command.unwrap_or(Command::Run) {
        Command::Crd => {
            let crds = [
                Bucket::crd(),
                User::crd(),
                Policy::crd(),
                AccessKey::crd(),
                ClusterConnection::crd(),
            ];
            let docs: Vec<String> = crds
                .iter()
                .map(serde_yaml::to_string)
                .collect::<Result<_, _>>()?;
            print!("{}", docs.join("---\n"));
        }
        Command::Run => {
            rustfs_operator::install_crypto_provider();
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info,kube=warn".into()),
                )
                .init();
            let client = kube::Client::try_default().await?;
            reconcile::run_all(client).await?;
        }
    }
    Ok(())
}

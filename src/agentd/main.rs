pub mod open_monitor;
pub mod control_plane;
pub mod sbom;

use anyhow::{Result, anyhow};
use log::*;
use clap::Parser;

use open_monitor::OpenEvent;

use edgebit_agent::packages::{Registry};

#[derive(Parser)]
struct CliArgs {
    #[clap(long = "sbom")]
    sbom: Option<String>,

    #[clap(long = "no-sbom-upload")]
    no_sbom_upload: bool,
}

#[tokio::main]
async fn main() {
    pretty_env_logger::init();

    let args = CliArgs::parse();
    match run(&args).await {
        Ok(_) => {},
        Err(err) => {
            eprintln!("err: {err}");
            eprintln!("src: {}", err.source().unwrap());
        }
    }
}

async fn run(args: &CliArgs) -> Result<()> {
    let url = std::env::var("EDGEBIT_URL")
        .map_err(|_| anyhow!("Is EDGEBIT_URL env var set?"))?;

    let token = std::env::var("EDGEBIT_ID")
        .map_err(|_| anyhow!("Is EDGEBIT_ID env var set?"))?;

    let sbom_doc = match &args.sbom {
        Some(sbom_path) => {
            info!("Loading SBOM");
            sbom::load(sbom_path)?
        },
        None => {
            info!("Generating SBOM");
            sbom::generate()?
        },
    };

    info!("Connecting to Edgebit at {url}");
    let mut client = control_plane::Client::connect(
        url.try_into()?,
        token.try_into()?,
    ).await?;

    let mut pkg_registry = Registry::from_sbom(&sbom_doc);

    if !args.no_sbom_upload {
        info!("Uploading SBOM to Edgebit");
        client.upload_sbom(json::stringify(sbom_doc)).await?;
    }

    info!("Starting to monitor packages in use");
    report_in_use(&mut client, &mut pkg_registry).await?;
    Ok(())
}

async fn report_in_use(client: &mut control_plane::Client, pkg_registry: &mut Registry) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<OpenEvent>(1000);
    let monitor_task = tokio::task::spawn_blocking(move || open_monitor::run(tx));

    // batch in 1s intervals

    while let Some(evt) = rx.recv().await {
        match evt.filename.into_string() {
            Ok(filename) => {
                let filenames = vec![filename];
                let pkgs = pkg_registry.get_packages(filenames);
                _ = client.report_in_use(pkgs).await;
            },

            Err(_) => (),
        }
    }

    monitor_task.await.unwrap().unwrap();

    Ok(())
}
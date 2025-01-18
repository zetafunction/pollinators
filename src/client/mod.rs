use anyhow::{anyhow, Result};
use console::style;
use std::io::Write;
use std::path::Path;
use std::process::Command;

pub trait Client {
    fn add_torrent(&self, torrent_path: &Path, seed_path: &Path) -> Result<()>;
}

struct Synapse;

impl Client for Synapse {
    fn add_torrent(&self, torrent_path: &Path, seed_path: &Path) -> Result<()> {
        let output = Command::new("sycli")
            .arg("add")
            .arg("-P")
            .arg("-d")
            .arg(seed_path)
            .arg(torrent_path)
            .output()?;

        let result = match output.status.code() {
            Some(0) => Ok(()),
            Some(code) => Err(anyhow!("client command exited with code {}", code)),
            None => Err(anyhow!("client terminated by signal")),
        };
        if result.is_err() {
            println!(
                "failed to add {} from {}",
                torrent_path.display(),
                seed_path.display()
            );
            std::io::stdout().write_all(&output.stdout).unwrap();
            std::io::stderr().write_all(&output.stderr).unwrap();
        }
        result
    }
}

struct DryRun;

impl Client for DryRun {
    fn add_torrent(&self, torrent_path: &Path, seed_path: &Path) -> Result<()> {
        println!(
            "{} {} {} {}",
            style("seeding").green(),
            style(torrent_path.display()).cyan(),
            style("from").green(),
            style(seed_path.display()).cyan()
        );
        Ok(())
    }
}

// TODO: Support more clients.
pub fn new_instance(dry_run: bool) -> Box<dyn Client> {
    if dry_run {
        Box::new(DryRun {})
    } else {
        Box::new(Synapse {})
    }
}

use anyhow::Result;

use fbd::config::{Config, Paths};

fn main() -> Result<()> {
    let paths = Paths::resolve()?;
    let config_file = paths.config_file();

    let roster = if config_file.exists() {
        Config::load(config_file)?
    } else {
        Config::default()
    };

    println!("fbd — federated beads (scaffold)");
    println!("config: {}", config_file.display());
    println!("data:   {}", paths.data_dir().display());
    println!("repos:  {}", roster.repos.len());

    Ok(())
}

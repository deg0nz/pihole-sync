use std::{env, fs, path::PathBuf};

use anyhow::{bail, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, Input};

fn quote_if_needed(value: &str) -> String {
    if value.contains(' ') {
        format!("\"{}\"", value)
    } else {
        value.to_string()
    }
}

pub fn create_default_config() -> Result<()> {
    let cwd = env::current_dir()?;
    let target_path = cwd.join("config.yaml");
    let theme = ColorfulTheme::default();

    if target_path.exists() {
        let overwrite = Confirm::with_theme(&theme)
            .with_prompt(format!(
                "{} already exists. Overwrite?",
                target_path.display()
            ))
            .default(false)
            .interact()?;

        if !overwrite {
            println!("Skipping config creation.");
            return Ok(());
        }
    }

    let default_config = include_str!("../../config/example.config.yaml");
    fs::write(&target_path, default_config)?;
    println!("Default config created at {}", target_path.display());

    Ok(())
}

pub fn create_systemd_service() -> Result<()> {
    let cwd = env::current_dir()?;
    let theme = ColorfulTheme::default();

    let default_config_path = cwd.join("config.yaml");
    let default_exec_path = cwd.join("pihole-sync");
    let default_service_path = PathBuf::from("/etc/systemd/system/pihole-sync.service");

    let config_path: String = Input::with_theme(&theme)
        .with_prompt("Path to pihole-sync config file")
        .default(default_config_path.display().to_string())
        .interact_text()?;

    let executable_path: String = Input::with_theme(&theme)
        .with_prompt("Path to pihole-sync executable")
        .default(default_exec_path.display().to_string())
        .interact_text()?;

    let install = Confirm::with_theme(&theme)
        .with_prompt("Install systemd service file now?")
        .default(true)
        .interact()?;

    let target_service_path: String = if install {
        Input::with_theme(&theme)
            .with_prompt("Systemd service destination")
            .default(default_service_path.display().to_string())
            .interact_text()?
    } else {
        cwd.join("pihole-sync.service").display().to_string()
    };

    let working_dir = cwd.display().to_string();
    let service_contents = format!(
        "[Unit]\nDescription=Pi-hole Sync Service\nAfter=network.target pihole-FTL.service\n\n[Service]\nWorkingDirectory={}\nRestart=always\nUser=pihole\nGroup=pihole\nEnvironment=\"RUST_LOG=info\"\nExecStart={} -c {} sync\n\n[Install]\nWantedBy=multi-user.target\n",
        quote_if_needed(&working_dir),
        quote_if_needed(&executable_path),
        quote_if_needed(&config_path)
    );

    let service_path = PathBuf::from(target_service_path);
    if let Some(parent) = service_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }

    if service_path.exists() {
        let overwrite = Confirm::with_theme(&theme)
            .with_prompt(format!(
                "{} already exists. Overwrite?",
                service_path.display()
            ))
            .default(false)
            .interact()?;

        if !overwrite {
            bail!("Aborted writing systemd service file.");
        }
    }

    fs::write(&service_path, service_contents)?;
    println!("Systemd service file written to {}", service_path.display());
    if install {
        println!("Enable with: sudo systemctl enable --now pihole-sync.service");
    }

    Ok(())
}

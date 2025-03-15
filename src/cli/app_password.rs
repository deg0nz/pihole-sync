use std::time::Duration;

use anyhow::Result;
use dialoguer::{theme::ColorfulTheme, Password, Select};
use indicatif::ProgressBar;

use crate::{
    config::{Config, InstanceConfig},
    pihole_client::{AppPassword, PiHoleClient},
};

pub async fn acquire_app_password(config_path: &str) -> Result<()> {
    let config = Config::load(config_path)?;
    let mut instances_list: Vec<InstanceConfig> = Vec::new();

    instances_list.push(config.main);

    for secondary in &config.secondary {
        instances_list.push(secondary.clone());
    }

    let selection_list: Vec<String> = instances_list
        .iter()
        .map(|instance| instance.host.clone())
        .collect();

    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Please select the instance to fetch an API app password from")
        .items(&selection_list)
        .interact()
        .unwrap();

    let password = Password::with_theme(&ColorfulTheme::default())
        .with_prompt(format!(
            "Please enter your Pi-hole webinterface password for {}",
            instances_list[selection].host
        ))
        .interact()
        .unwrap();

    let pihole_client = PiHoleClient::new(instances_list[selection].clone());

    let bar = ProgressBar::new_spinner();
    bar.enable_steady_tick(Duration::from_millis(100));
    let app_pw: AppPassword = pihole_client.fetch_app_password(password).await?;
    bar.finish();

    println!(
        "🎉 Successfully fetched API app password for {}",
        instances_list[selection].host
    );
    println!("Password (add to pihole-sync config): {}", app_pw.password);
    println!("Hash (add to Pi-hole): {}", app_pw.hash);
    println!("");
    println!("-----");
    println!("Hint:");
    println!(
        "Add the password to the pihole-sync configuration for the instance {}.",
        instances_list[selection].host
    );
    println!(
        "You need to add the hash to Settings > Webserver and API > webserver.api.app_pwhash in the Pi-hole web interface."
    );
    println!("Refer to Pi-hole API documentation for more information: https://ftl.pi-hole.net/master/docs/#get-/auth/app");

    pihole_client.logout().await?;

    Ok(())
}

mod config;
mod db;
mod discord;
mod entries;
mod init;
mod members;

use std::env;
use std::sync::{Arc, Mutex};

use clap::{Parser, Subcommand};
use config::Config;

#[cfg(test)]
pub(crate) mod test_support {
    /// Shared across every module's tests that mutate process env vars
    /// (`DISPATCHD_CONFIG_PATH`, `DISPATCHD_DB_PATH`, `DISPATCHD_MEMBERS_PATH`,
    /// `DISPATCHD_DISCORD_TOKEN`). `cargo test` runs tests in parallel
    /// within one process, and these vars overlap across
    /// config.rs/members.rs/init.rs/main.rs tests, so a single crate-wide
    /// lock is required rather than one lock per file.
    pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Write default config.toml and members.toml templates if missing
    Init,
}

/// Returns the bot token and guild ID needed to start the Discord client,
/// or `None` if either is missing - not configured yet is a valid state,
/// not an error, so `dispatchd` stays useful for config/DB setup before a
/// token exists.
fn discord_credentials(config: &Config) -> Option<(String, u64)> {
    let token = env::var("DISPATCHD_DISCORD_TOKEN").ok()?;
    let guild_id = config.discord_guild_id?;
    Some((token, guild_id))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if matches!(cli.command, Some(Command::Init)) {
        return init::run();
    }

    let config = Config::load()?;
    let conn = db::open(&config.db_path)?;
    let seeded = members::seed(&conn)?;

    println!("dispatchd effective schedule:");
    println!("  todo_time:                    {}", config.todo_time);
    println!("  update_time:                  {}", config.update_time);
    println!(
        "  meeting_reminder_time:        {}",
        config.meeting_reminder_time
    );
    println!(
        "  todo_followup_delay_minutes:  {}",
        config.todo_followup_delay_minutes
    );
    println!(
        "  update_followup_delay_minutes: {}",
        config.update_followup_delay_minutes
    );
    println!(
        "  ticker_interval_seconds:      {}",
        config.ticker_interval_seconds
    );
    println!("  timezone:                     {}", config.timezone);
    println!(
        "  db_path:                      {}",
        config.db_path.display()
    );
    if seeded > 0 {
        println!("  members seeded:               {seeded}");
    } else {
        println!(
            "  members seeded:               0 (no members configured yet — run `dispatchd init`, then edit members.toml)"
        );
    }
    match config.discord_guild_id {
        Some(id) => println!("  discord_guild_id:             {id}"),
        None => println!("  discord_guild_id:             not configured"),
    }

    match discord_credentials(&config) {
        Some((token, guild_id)) => {
            let db = Arc::new(Mutex::new(conn));
            discord::run(token, guild_id, config.timezone, db).await?
        }
        None => println!(
            "Discord not configured — see docs/discord-setup.md to set DISPATCHD_DISCORD_TOKEN and discord_guild_id"
        ),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ENV_LOCK;

    #[test]
    fn both_present_yields_credentials() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = Config {
            discord_guild_id: Some(42),
            ..Config::default()
        };
        // SAFETY: held under ENV_LOCK; removed before returning.
        unsafe {
            env::set_var("DISPATCHD_DISCORD_TOKEN", "test-token");
        }
        let result = discord_credentials(&config);
        unsafe {
            env::remove_var("DISPATCHD_DISCORD_TOKEN");
        }
        assert_eq!(result, Some(("test-token".to_string(), 42)));
    }

    #[test]
    fn missing_token_yields_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = Config {
            discord_guild_id: Some(42),
            ..Config::default()
        };
        // SAFETY: held under ENV_LOCK.
        unsafe {
            env::remove_var("DISPATCHD_DISCORD_TOKEN");
        }
        assert_eq!(discord_credentials(&config), None);
    }

    #[test]
    fn missing_guild_id_yields_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = Config::default();
        // SAFETY: held under ENV_LOCK; removed before returning.
        unsafe {
            env::set_var("DISPATCHD_DISCORD_TOKEN", "test-token");
        }
        let result = discord_credentials(&config);
        unsafe {
            env::remove_var("DISPATCHD_DISCORD_TOKEN");
        }
        assert_eq!(result, None);
    }
}

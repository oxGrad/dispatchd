mod config;
mod db;
mod discord;
mod discord_login;
mod entries;
mod followups;
mod init;
mod lock;
mod maintenance;
mod members;
mod reminders;
mod service;
mod status;

use std::env;
use std::sync::{Arc, Mutex};

use clap::{Parser, Subcommand};
use config::Config;

#[cfg(test)]
pub(crate) mod test_support {
    /// Shared across every module's tests that mutate process env vars
    /// (`DISPATCHD_CONFIG_PATH`, `DISPATCHD_DB_PATH`, `DISPATCHD_MEMBERS_PATH`,
    /// `DISPATCHD_DISCORD_TOKEN`, `CREDENTIALS_DIRECTORY`). `cargo test`
    /// runs tests in parallel within one process, and these vars overlap
    /// across config.rs/members.rs/init.rs/main.rs tests, so a single
    /// crate-wide lock is required rather than one lock per file.
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
    /// Manage the Discord bot token
    Discord {
        #[command(subcommand)]
        action: DiscordCommand,
    },
    /// Manage the systemd service (Linux only)
    Service {
        #[command(subcommand)]
        action: ServiceCommand,
    },
    /// Database maintenance (the handover doc's weekly cron)
    Maintenance {
        #[command(subcommand)]
        action: MaintenanceCommand,
    },
    /// Check the systemd service and Discord connectivity
    Status,
}

#[derive(Subcommand)]
enum DiscordCommand {
    /// Log in interactively: prompts for the bot token, validates it
    /// against Discord, and encrypts it at rest via systemd-creds
    Login,
}

#[derive(Subcommand)]
enum ServiceCommand {
    /// Install the systemd unit and enable it to start at boot
    Install,
}

#[derive(Subcommand)]
enum MaintenanceCommand {
    /// Prune reminders_sent/followups_sent rows older than 90 days and
    /// VACUUM the database. entries is never touched.
    Run,
}

/// Returns the bot token and guild ID needed to start the Discord client,
/// or `None` if either is missing - not configured yet is a valid state,
/// not an error, so `dispatchd` stays useful for config/DB setup before a
/// token exists.
fn discord_credentials(config: &Config) -> Option<(String, u64)> {
    let token = discord_token()?;
    let guild_id = config.discord_guild_id?;
    Some((token, guild_id))
}

/// Resolves the Discord bot token. Prefers the systemd-decrypted
/// credential at `$CREDENTIALS_DIRECTORY/discord_token` - set for units
/// using `LoadCredentialEncrypted=discord_token:...` (see
/// `service::install`), which is how `dispatchd discord login` wires the
/// token up encrypted-at-rest via `systemd-creds` rather than as
/// plaintext. Falls back to `DISPATCHD_DISCORD_TOKEN` for local/dev runs
/// outside systemd, where there's no credentials directory to read from.
fn discord_token() -> Option<String> {
    if let Ok(dir) = env::var("CREDENTIALS_DIRECTORY") {
        let path = std::path::Path::new(&dir).join("discord_token");
        if let Ok(contents) = std::fs::read_to_string(&path) {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    env::var("DISPATCHD_DISCORD_TOKEN").ok()
}

async fn run_status() -> anyhow::Result<()> {
    service::status()?;
    discord_login::ping().await;
    Ok(())
}

fn run_maintenance() -> anyhow::Result<()> {
    let config = Config::load()?;

    // A separate lock from the main run's `<db>.lock` - the weekly
    // maintenance timer is designed to fire independently of (and
    // concurrently with) the long-running bot process, so this only
    // guards against two overlapping `maintenance run` invocations, e.g.
    // a slow run still going when the next timer fires.
    let _singleton = lock::acquire(&config.db_path.with_extension("maintenance.lock"))?;

    let conn = db::open(&config.db_path)?;
    let (reminders_deleted, followups_deleted) = maintenance::run(&conn)?;
    println!(
        "maintenance done: {reminders_deleted} old reminders_sent row(s), \
         {followups_deleted} old followups_sent row(s) pruned, database vacuumed"
    );
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Init) => return init::run(),
        Some(Command::Discord {
            action: DiscordCommand::Login,
        }) => return discord_login::run().await,
        Some(Command::Service {
            action: ServiceCommand::Install,
        }) => return service::install(),
        Some(Command::Maintenance {
            action: MaintenanceCommand::Run,
        }) => return run_maintenance(),
        Some(Command::Status) => return run_status().await,
        None => {}
    }

    let config = Config::load()?;

    // Held for the rest of `main` - guards against two dispatchd processes
    // racing the ticker against the same data directory (see src/lock.rs).
    let _singleton = lock::acquire(&config.db_path.with_extension("lock"))?;

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
            discord::run(token, guild_id, config.clone(), db).await?
        }
        None => println!(
            "Discord not configured — run `sudo dispatchd discord login` (or set DISPATCHD_DISCORD_TOKEN) and set discord_guild_id; see docs/discord-setup.md"
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

    #[test]
    fn credentials_directory_token_wins_over_env_var() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("discord_token"), "creds-token\n").unwrap();
        // SAFETY: held under ENV_LOCK; both vars removed before returning.
        unsafe {
            env::set_var("CREDENTIALS_DIRECTORY", dir.path());
            env::set_var("DISPATCHD_DISCORD_TOKEN", "env-token");
        }
        let result = discord_token();
        unsafe {
            env::remove_var("CREDENTIALS_DIRECTORY");
            env::remove_var("DISPATCHD_DISCORD_TOKEN");
        }
        assert_eq!(result, Some("creds-token".to_string()));
    }

    #[test]
    fn missing_credential_file_falls_back_to_env_var() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: held under ENV_LOCK; both vars removed before returning.
        unsafe {
            env::set_var("CREDENTIALS_DIRECTORY", dir.path());
            env::set_var("DISPATCHD_DISCORD_TOKEN", "env-token");
        }
        let result = discord_token();
        unsafe {
            env::remove_var("CREDENTIALS_DIRECTORY");
            env::remove_var("DISPATCHD_DISCORD_TOKEN");
        }
        assert_eq!(result, Some("env-token".to_string()));
    }

    #[test]
    fn neither_source_yields_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: held under ENV_LOCK.
        unsafe {
            env::remove_var("CREDENTIALS_DIRECTORY");
            env::remove_var("DISPATCHD_DISCORD_TOKEN");
        }
        assert_eq!(discord_token(), None);
    }
}

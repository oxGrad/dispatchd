mod config;
mod db;
mod init;
mod members;

use clap::{Parser, Subcommand};
use config::Config;

#[cfg(test)]
pub(crate) mod test_support {
    /// Shared across every module's tests that mutate process env vars
    /// (`DISPATCHD_CONFIG_PATH`, `DISPATCHD_DB_PATH`, `DISPATCHD_MEMBERS_PATH`).
    /// `cargo test` runs tests in parallel within one process, and these
    /// vars overlap across config.rs/members.rs/init.rs tests, so a single
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
}

fn main() -> anyhow::Result<()> {
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

    Ok(())
}

mod config;

use config::Config;

fn main() -> anyhow::Result<()> {
    let config = Config::load()?;
    println!("dispatchd effective schedule:");
    println!("  todo_time:                  {}", config.todo_time);
    println!("  update_time:                {}", config.update_time);
    println!("  meeting_reminder_time:      {}", config.meeting_reminder_time);
    println!(
        "  todo_followup_delay_minutes: {}",
        config.todo_followup_delay_minutes
    );
    println!(
        "  ticker_interval_seconds:     {}",
        config.ticker_interval_seconds
    );
    println!("  timezone:                   {}", config.timezone);
    Ok(())
}

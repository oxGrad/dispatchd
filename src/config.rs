use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::NaiveTime;
use chrono_tz::Tz;
use serde::Deserialize;

/// Env var that, when set, points directly at the config file to load,
/// bypassing XDG lookup. Useful under systemd where HOME/XDG_CONFIG_HOME
/// may not be populated for a service account.
const CONFIG_PATH_OVERRIDE_ENV: &str = "DISPATCHD_CONFIG_PATH";

const DEFAULT_TODO_TIME: &str = "09:00";
const DEFAULT_UPDATE_TIME: &str = "15:00";
const DEFAULT_MEETING_REMINDER_TIME: &str = "16:00";
const DEFAULT_TODO_FOLLOWUP_DELAY_MINUTES: u32 = 30;
const DEFAULT_TICKER_INTERVAL_SECONDS: u64 = 60;
const DEFAULT_TIMEZONE: &str = "UTC";

/// The resolved, fully-typed schedule config actually used by the rest of
/// the program. Every field is guaranteed valid.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub todo_time: NaiveTime,
    pub update_time: NaiveTime,
    pub meeting_reminder_time: NaiveTime,
    pub todo_followup_delay_minutes: u32,
    pub ticker_interval_seconds: u64,
    pub timezone: Tz,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            todo_time: parse_time(DEFAULT_TODO_TIME).expect("default todo_time is valid"),
            update_time: parse_time(DEFAULT_UPDATE_TIME).expect("default update_time is valid"),
            meeting_reminder_time: parse_time(DEFAULT_MEETING_REMINDER_TIME)
                .expect("default meeting_reminder_time is valid"),
            todo_followup_delay_minutes: DEFAULT_TODO_FOLLOWUP_DELAY_MINUTES,
            ticker_interval_seconds: DEFAULT_TICKER_INTERVAL_SECONDS,
            timezone: parse_timezone(DEFAULT_TIMEZONE).expect("default timezone is valid"),
        }
    }
}

/// Mirrors exactly what's optional in the TOML file. A missing or empty
/// file deserializes to all-`None`, which is a valid state (defaults only).
#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    todo_time: Option<String>,
    update_time: Option<String>,
    meeting_reminder_time: Option<String>,
    todo_followup_delay_minutes: Option<u32>,
    ticker_interval_seconds: Option<u64>,
    timezone: Option<String>,
}

impl Config {
    /// Loads the effective config: hardcoded defaults merged with any
    /// overrides found in the XDG-located (or explicitly pointed-at) config
    /// file. A missing file is not an error - it just means "use defaults".
    pub fn load() -> Result<Config> {
        let raw = match find_config_path()? {
            Some(path) => read_raw_config(&path)?,
            None => RawConfig::default(),
        };
        Config::from_raw(raw)
    }

    fn from_raw(raw: RawConfig) -> Result<Config> {
        let defaults = Config::default();

        let todo_time = match raw.todo_time {
            Some(s) => parse_time(&s).with_context(|| format!("invalid todo_time: {s:?}"))?,
            None => defaults.todo_time,
        };
        let update_time = match raw.update_time {
            Some(s) => parse_time(&s).with_context(|| format!("invalid update_time: {s:?}"))?,
            None => defaults.update_time,
        };
        let meeting_reminder_time = match raw.meeting_reminder_time {
            Some(s) => parse_time(&s)
                .with_context(|| format!("invalid meeting_reminder_time: {s:?}"))?,
            None => defaults.meeting_reminder_time,
        };
        let timezone = match raw.timezone {
            Some(s) => parse_timezone(&s).with_context(|| format!("invalid timezone: {s:?}"))?,
            None => defaults.timezone,
        };

        Ok(Config {
            todo_time,
            update_time,
            meeting_reminder_time,
            todo_followup_delay_minutes: raw
                .todo_followup_delay_minutes
                .unwrap_or(defaults.todo_followup_delay_minutes),
            ticker_interval_seconds: raw
                .ticker_interval_seconds
                .unwrap_or(defaults.ticker_interval_seconds),
            timezone,
        })
    }
}

fn parse_time(s: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M").with_context(|| format!("expected HH:MM, got {s:?}"))
}

fn parse_timezone(s: &str) -> Result<Tz> {
    s.parse::<Tz>()
        .map_err(|e| anyhow::anyhow!("unknown IANA timezone {s:?}: {e}"))
}

/// Resolves the config file path to load, without reading it. Returns
/// `None` when no override env var is set and no XDG config file exists.
fn find_config_path() -> Result<Option<PathBuf>> {
    if let Ok(path) = env::var(CONFIG_PATH_OVERRIDE_ENV) {
        return Ok(Some(PathBuf::from(path)));
    }

    let xdg_dirs = xdg::BaseDirectories::with_prefix("dispatchd");
    Ok(xdg_dirs.find_config_file("config.toml"))
}

fn read_raw_config(path: &Path) -> Result<RawConfig> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    toml::from_str(&contents)
        .with_context(|| format!("failed to parse config file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_config(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        (dir, path)
    }

    #[test]
    fn missing_file_yields_defaults() {
        let config = Config::from_raw(RawConfig::default()).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn partial_override_changes_only_that_field() {
        let (_dir, path) = write_config("update_time = \"18:30\"\n");
        let raw = read_raw_config(&path).unwrap();
        let config = Config::from_raw(raw).unwrap();

        let defaults = Config::default();
        assert_eq!(config.update_time, NaiveTime::from_hms_opt(18, 30, 0).unwrap());
        assert_eq!(config.todo_time, defaults.todo_time);
        assert_eq!(config.meeting_reminder_time, defaults.meeting_reminder_time);
        assert_eq!(
            config.todo_followup_delay_minutes,
            defaults.todo_followup_delay_minutes
        );
        assert_eq!(config.ticker_interval_seconds, defaults.ticker_interval_seconds);
        assert_eq!(config.timezone, defaults.timezone);
    }

    #[test]
    fn full_override_changes_every_field() {
        let (_dir, path) = write_config(
            r#"
            todo_time = "08:15"
            update_time = "14:45"
            meeting_reminder_time = "17:00"
            todo_followup_delay_minutes = 45
            ticker_interval_seconds = 120
            timezone = "Asia/Jakarta"
            "#,
        );
        let raw = read_raw_config(&path).unwrap();
        let config = Config::from_raw(raw).unwrap();

        assert_eq!(config.todo_time, NaiveTime::from_hms_opt(8, 15, 0).unwrap());
        assert_eq!(config.update_time, NaiveTime::from_hms_opt(14, 45, 0).unwrap());
        assert_eq!(
            config.meeting_reminder_time,
            NaiveTime::from_hms_opt(17, 0, 0).unwrap()
        );
        assert_eq!(config.todo_followup_delay_minutes, 45);
        assert_eq!(config.ticker_interval_seconds, 120);
        assert_eq!(config.timezone, Tz::Asia__Jakarta);
    }

    #[test]
    fn invalid_time_string_is_an_error() {
        let (_dir, path) = write_config("todo_time = \"25:00\"\n");
        let raw = read_raw_config(&path).unwrap();
        let err = Config::from_raw(raw).unwrap_err();
        assert!(err.to_string().contains("todo_time"), "{err}");
    }

    #[test]
    fn invalid_timezone_string_is_an_error() {
        let (_dir, path) = write_config("timezone = \"Not/AZone\"\n");
        let raw = read_raw_config(&path).unwrap();
        let err = Config::from_raw(raw).unwrap_err();
        assert!(err.to_string().contains("timezone"), "{err}");
    }

    #[test]
    fn config_path_override_env_var_is_honored() {
        let (_dir, path) = write_config("update_time = \"20:00\"\n");
        // SAFETY: tests run single-threaded within this process for env var
        // mutation purposes is not guaranteed by cargo test, but this test
        // is self-contained: it only reads back its own value immediately.
        unsafe {
            env::set_var(CONFIG_PATH_OVERRIDE_ENV, &path);
        }
        let resolved = find_config_path().unwrap();
        unsafe {
            env::remove_var(CONFIG_PATH_OVERRIDE_ENV);
        }
        assert_eq!(resolved, Some(path));
    }
}

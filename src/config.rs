use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::NaiveTime;
use chrono_tz::Tz;
use serde::Deserialize;

/// Env var that, when set, points directly at the config file to load,
/// bypassing XDG lookup. Useful under systemd where HOME/XDG_CONFIG_HOME
/// may not be populated for a service account.
pub const CONFIG_PATH_OVERRIDE_ENV: &str = "DISPATCHD_CONFIG_PATH";
/// Env var that, when set, points directly at the SQLite DB file to open,
/// bypassing the XDG data-dir default.
const DB_PATH_OVERRIDE_ENV: &str = "DISPATCHD_DB_PATH";

const DEFAULT_THREAD_CREATION_TIME: &str = "08:30";
const DEFAULT_TODO_TIME: &str = "09:00";
const DEFAULT_UPDATE_TIME: &str = "15:00";
const DEFAULT_MEETING_TIME: &str = "16:00";
const DEFAULT_MEETING_REMINDER_LEAD_MINUTES: u32 = 5;
const DEFAULT_TODO_FOLLOWUP_DELAY_MINUTES: u32 = 30;
const DEFAULT_UPDATE_FOLLOWUP_DELAY_MINUTES: u32 = 30;
const DEFAULT_TICKER_INTERVAL_SECONDS: u64 = 60;
const DEFAULT_RUN_ON_WEEKENDS: bool = false;
const DEFAULT_TIMEZONE: &str = "UTC";
const DEFAULT_DB_FILE_NAME: &str = "dispatchd.sqlite3";

/// The resolved, fully-typed, flat config actually used by the rest of the
/// program. Every field is guaranteed valid.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub thread_creation_time: NaiveTime,
    pub todo_time: NaiveTime,
    pub update_time: NaiveTime,
    /// When the daily meeting actually starts.
    pub meeting_time: NaiveTime,
    /// How many minutes before `meeting_time` the pre-meeting reminder fires.
    pub meeting_reminder_lead_minutes: u32,
    pub todo_followup_delay_minutes: u32,
    pub update_followup_delay_minutes: u32,
    pub ticker_interval_seconds: u64,
    pub run_on_weekends: bool,
    pub timezone: Tz,
    pub db_path: PathBuf,
    pub discord_guild_id: Option<u64>,
    pub discord_standup_channel_id: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            thread_creation_time: parse_time(DEFAULT_THREAD_CREATION_TIME)
                .expect("default thread_creation_time is valid"),
            todo_time: parse_time(DEFAULT_TODO_TIME).expect("default todo_time is valid"),
            update_time: parse_time(DEFAULT_UPDATE_TIME).expect("default update_time is valid"),
            meeting_time: parse_time(DEFAULT_MEETING_TIME).expect("default meeting_time is valid"),
            meeting_reminder_lead_minutes: DEFAULT_MEETING_REMINDER_LEAD_MINUTES,
            todo_followup_delay_minutes: DEFAULT_TODO_FOLLOWUP_DELAY_MINUTES,
            update_followup_delay_minutes: DEFAULT_UPDATE_FOLLOWUP_DELAY_MINUTES,
            ticker_interval_seconds: DEFAULT_TICKER_INTERVAL_SECONDS,
            run_on_weekends: DEFAULT_RUN_ON_WEEKENDS,
            timezone: parse_timezone(DEFAULT_TIMEZONE).expect("default timezone is valid"),
            db_path: xdg_default_db_path().expect("default db_path is resolvable"),
            discord_guild_id: None,
            discord_standup_channel_id: None,
        }
    }
}

/// Mirrors exactly what's optional in the TOML file. A missing or empty
/// file deserializes to all-`None`, which is a valid state (defaults only).
#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    schedule: RawSchedule,
    #[serde(default)]
    followup: RawFollowup,
    timezone: Option<String>,
    db_path: Option<String>,
    discord_guild_id: Option<u64>,
    discord_standup_channel_id: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct RawSchedule {
    thread_creation_time: Option<String>,
    todo_time: Option<String>,
    update_time: Option<String>,
    meeting_time: Option<String>,
    meeting_reminder_lead_minutes: Option<u32>,
    ticker_interval_seconds: Option<u64>,
    run_on_weekends: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct RawFollowup {
    todo_delay_minutes: Option<u32>,
    update_delay_minutes: Option<u32>,
}

impl Config {
    /// Loads the effective config: hardcoded defaults merged with any
    /// overrides found in the XDG-located (or explicitly pointed-at) config
    /// file. A missing file is not an error - it just means "use defaults".
    pub fn load() -> Result<Config> {
        let raw = match config_file_path()? {
            Some(path) => read_raw_config(&path)?,
            None => RawConfig::default(),
        };
        let mut config = Config::from_raw(raw)?;
        // Highest-precedence override, applied once here rather than inside
        // `from_raw`/`Default` - keeps those two env-var-free and safe to
        // call from concurrent tests.
        if let Ok(path) = env::var(DB_PATH_OVERRIDE_ENV) {
            config.db_path = PathBuf::from(path);
        }
        Ok(config)
    }

    fn from_raw(raw: RawConfig) -> Result<Config> {
        let defaults = Config::default();

        let thread_creation_time = match raw.schedule.thread_creation_time {
            Some(s) => parse_time(&s)
                .with_context(|| format!("invalid schedule.thread_creation_time: {s:?}"))?,
            None => defaults.thread_creation_time,
        };
        let todo_time = match raw.schedule.todo_time {
            Some(s) => {
                parse_time(&s).with_context(|| format!("invalid schedule.todo_time: {s:?}"))?
            }
            None => defaults.todo_time,
        };
        let update_time = match raw.schedule.update_time {
            Some(s) => {
                parse_time(&s).with_context(|| format!("invalid schedule.update_time: {s:?}"))?
            }
            None => defaults.update_time,
        };
        let meeting_time = match raw.schedule.meeting_time {
            Some(s) => {
                parse_time(&s).with_context(|| format!("invalid schedule.meeting_time: {s:?}"))?
            }
            None => defaults.meeting_time,
        };
        let timezone = match raw.timezone {
            Some(s) => parse_timezone(&s).with_context(|| format!("invalid timezone: {s:?}"))?,
            None => defaults.timezone,
        };
        let db_path = match raw.db_path {
            Some(s) => PathBuf::from(s),
            None => defaults.db_path,
        };

        Ok(Config {
            thread_creation_time,
            todo_time,
            update_time,
            meeting_time,
            meeting_reminder_lead_minutes: raw
                .schedule
                .meeting_reminder_lead_minutes
                .unwrap_or(defaults.meeting_reminder_lead_minutes),
            todo_followup_delay_minutes: raw
                .followup
                .todo_delay_minutes
                .unwrap_or(defaults.todo_followup_delay_minutes),
            update_followup_delay_minutes: raw
                .followup
                .update_delay_minutes
                .unwrap_or(defaults.update_followup_delay_minutes),
            ticker_interval_seconds: raw
                .schedule
                .ticker_interval_seconds
                .unwrap_or(defaults.ticker_interval_seconds),
            run_on_weekends: raw
                .schedule
                .run_on_weekends
                .unwrap_or(defaults.run_on_weekends),
            timezone,
            db_path,
            discord_guild_id: raw.discord_guild_id,
            discord_standup_channel_id: raw.discord_standup_channel_id,
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

pub(crate) fn xdg_dirs() -> xdg::BaseDirectories {
    xdg::BaseDirectories::with_prefix("dispatchd")
}

/// Resolves the config file path to load, without reading it. Returns
/// `None` when no override env var is set and no XDG config file exists.
pub fn config_file_path() -> Result<Option<PathBuf>> {
    if let Ok(path) = env::var(CONFIG_PATH_OVERRIDE_ENV) {
        return Ok(Some(PathBuf::from(path)));
    }
    Ok(xdg_dirs().find_config_file("config.toml"))
}

/// Resolves the config file's *target* path for `dispatchd init` - i.e.
/// where a new file should be created, as opposed to `config_file_path`
/// which only locates one that already exists.
pub fn config_target_path() -> Result<PathBuf> {
    if let Ok(path) = env::var(CONFIG_PATH_OVERRIDE_ENV) {
        let path = PathBuf::from(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        return Ok(path);
    }
    xdg_dirs()
        .place_config_file("config.toml")
        .context("failed to resolve config.toml target path (is $HOME set?)")
}

/// Resolves the DB file's default location: $XDG_DATA_HOME/dispatchd/dispatchd.sqlite3,
/// without creating any directories (that's `db::open`'s job, at the point
/// it actually opens the file) and without consulting `DISPATCHD_DB_PATH`
/// (that's `Config::load`'s job, applied once at the very end).
fn xdg_default_db_path() -> Result<PathBuf> {
    xdg_dirs()
        .get_data_file(DEFAULT_DB_FILE_NAME)
        .context("could not determine data directory (is $HOME set?); set DISPATCHD_DB_PATH")
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
    use crate::test_support::ENV_LOCK;
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
    fn partial_schedule_override_changes_only_that_field() {
        let (_dir, path) = write_config("[schedule]\nupdate_time = \"18:30\"\n");
        let raw = read_raw_config(&path).unwrap();
        let config = Config::from_raw(raw).unwrap();

        let defaults = Config::default();
        assert_eq!(
            config.update_time,
            NaiveTime::from_hms_opt(18, 30, 0).unwrap()
        );
        assert_eq!(config.thread_creation_time, defaults.thread_creation_time);
        assert_eq!(config.todo_time, defaults.todo_time);
        assert_eq!(config.meeting_time, defaults.meeting_time);
        assert_eq!(
            config.meeting_reminder_lead_minutes,
            defaults.meeting_reminder_lead_minutes
        );
        assert_eq!(
            config.todo_followup_delay_minutes,
            defaults.todo_followup_delay_minutes
        );
        assert_eq!(
            config.update_followup_delay_minutes,
            defaults.update_followup_delay_minutes
        );
        assert_eq!(
            config.ticker_interval_seconds,
            defaults.ticker_interval_seconds
        );
        assert_eq!(config.run_on_weekends, defaults.run_on_weekends);
        assert_eq!(config.timezone, defaults.timezone);
        assert_eq!(config.db_path, defaults.db_path);
    }

    #[test]
    fn ritual_does_not_run_on_weekends_by_default() {
        assert!(!Config::default().run_on_weekends);
    }

    #[test]
    fn run_on_weekends_is_overridable() {
        let (_dir, path) = write_config("[schedule]\nrun_on_weekends = true\n");
        let raw = read_raw_config(&path).unwrap();
        let config = Config::from_raw(raw).unwrap();

        assert!(config.run_on_weekends);
    }

    #[test]
    fn thread_creation_time_is_overridable_independently() {
        let (_dir, path) = write_config("[schedule]\nthread_creation_time = \"07:45\"\n");
        let raw = read_raw_config(&path).unwrap();
        let config = Config::from_raw(raw).unwrap();

        let defaults = Config::default();
        assert_eq!(
            config.thread_creation_time,
            NaiveTime::from_hms_opt(7, 45, 0).unwrap()
        );
        assert_eq!(config.todo_time, defaults.todo_time);
    }

    #[test]
    fn full_override_changes_every_field() {
        let (_dir, path) = write_config(
            r#"
            timezone = "Asia/Jakarta"
            db_path = "/tmp/custom/dispatchd.sqlite3"
            discord_guild_id = 111111111111111111
            discord_standup_channel_id = 222222222222222222

            [schedule]
            thread_creation_time = "08:00"
            todo_time = "08:15"
            update_time = "14:45"
            meeting_time = "17:00"
            meeting_reminder_lead_minutes = 10
            ticker_interval_seconds = 120
            run_on_weekends = true

            [followup]
            todo_delay_minutes = 45
            update_delay_minutes = 20
            "#,
        );
        let raw = read_raw_config(&path).unwrap();
        let config = Config::from_raw(raw).unwrap();

        assert_eq!(
            config.thread_creation_time,
            NaiveTime::from_hms_opt(8, 0, 0).unwrap()
        );
        assert_eq!(config.todo_time, NaiveTime::from_hms_opt(8, 15, 0).unwrap());
        assert_eq!(
            config.update_time,
            NaiveTime::from_hms_opt(14, 45, 0).unwrap()
        );
        assert_eq!(
            config.meeting_time,
            NaiveTime::from_hms_opt(17, 0, 0).unwrap()
        );
        assert_eq!(config.meeting_reminder_lead_minutes, 10);
        assert_eq!(config.todo_followup_delay_minutes, 45);
        assert_eq!(config.update_followup_delay_minutes, 20);
        assert_eq!(config.ticker_interval_seconds, 120);
        assert!(config.run_on_weekends);
        assert_eq!(config.timezone, Tz::Asia__Jakarta);
        assert_eq!(
            config.db_path,
            PathBuf::from("/tmp/custom/dispatchd.sqlite3")
        );
        assert_eq!(config.discord_guild_id, Some(111111111111111111));
        assert_eq!(config.discord_standup_channel_id, Some(222222222222222222));
    }

    #[test]
    fn discord_ids_default_to_none() {
        let config = Config::from_raw(RawConfig::default()).unwrap();
        assert_eq!(config.discord_guild_id, None);
        assert_eq!(config.discord_standup_channel_id, None);
    }

    #[test]
    fn meeting_reminder_lead_defaults_to_five_and_is_overridable() {
        assert_eq!(Config::default().meeting_reminder_lead_minutes, 5);

        let (_dir, path) = write_config("[schedule]\nmeeting_reminder_lead_minutes = 15\n");
        let raw = read_raw_config(&path).unwrap();
        let config = Config::from_raw(raw).unwrap();

        assert_eq!(config.meeting_reminder_lead_minutes, 15);
        assert_eq!(config.meeting_time, Config::default().meeting_time);
    }

    #[test]
    fn followup_delays_resolve_independently() {
        let (_dir, path) = write_config("[followup]\ntodo_delay_minutes = 99\n");
        let raw = read_raw_config(&path).unwrap();
        let config = Config::from_raw(raw).unwrap();

        assert_eq!(config.todo_followup_delay_minutes, 99);
        assert_eq!(
            config.update_followup_delay_minutes,
            Config::default().update_followup_delay_minutes
        );
    }

    #[test]
    fn invalid_time_string_is_an_error() {
        let (_dir, path) = write_config("[schedule]\ntodo_time = \"25:00\"\n");
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
        let _guard = ENV_LOCK.lock().unwrap();
        let (_dir, path) = write_config("[schedule]\nupdate_time = \"20:00\"\n");
        // SAFETY: held under ENV_LOCK, so no concurrent test observes or
        // mutates this env var while it's set.
        unsafe {
            env::set_var(CONFIG_PATH_OVERRIDE_ENV, &path);
        }
        let resolved = config_file_path().unwrap();
        unsafe {
            env::remove_var(CONFIG_PATH_OVERRIDE_ENV);
        }
        assert_eq!(resolved, Some(path));
    }

    #[test]
    fn db_path_override_env_var_wins_over_config_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (_dir, config_path) = write_config("db_path = \"/from/config/file.sqlite3\"\n");
        // SAFETY: held under ENV_LOCK; both vars removed before returning.
        unsafe {
            env::set_var(CONFIG_PATH_OVERRIDE_ENV, &config_path);
            env::set_var(DB_PATH_OVERRIDE_ENV, "/from/env/file.sqlite3");
        }
        let config = Config::load().unwrap();
        unsafe {
            env::remove_var(CONFIG_PATH_OVERRIDE_ENV);
            env::remove_var(DB_PATH_OVERRIDE_ENV);
        }
        assert_eq!(config.db_path, PathBuf::from("/from/env/file.sqlite3"));
    }

    #[test]
    fn config_file_db_path_used_when_env_var_absent() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (_dir, config_path) = write_config("db_path = \"/from/config/file.sqlite3\"\n");
        // SAFETY: held under ENV_LOCK; removed before returning.
        unsafe {
            env::set_var(CONFIG_PATH_OVERRIDE_ENV, &config_path);
        }
        let config = Config::load().unwrap();
        unsafe {
            env::remove_var(CONFIG_PATH_OVERRIDE_ENV);
        }
        assert_eq!(config.db_path, PathBuf::from("/from/config/file.sqlite3"));
    }
}

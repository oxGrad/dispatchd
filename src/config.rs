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

const DEFAULT_THREAD_CREATION_TIME: &str = "07:00";
const DEFAULT_TODO_TIME: &str = "09:00";
const DEFAULT_UPDATE_TIME: &str = "15:00";
const DEFAULT_MEETING_TIME: &str = "16:00";
const DEFAULT_DAY_SUMMARY_TIME: &str = "16:00";
const DEFAULT_MEETING_REMINDER_LEAD_MINUTES: u32 = 5;
const DEFAULT_TODO_FOLLOWUP_DELAY_MINUTES: u32 = 30;
const DEFAULT_UPDATE_FOLLOWUP_DELAY_MINUTES: u32 = 30;
const DEFAULT_TICKER_INTERVAL_SECONDS: u64 = 60;
const DEFAULT_RUN_ON_WEEKENDS: bool = false;
const DEFAULT_TIMEZONE: &str = "UTC";
const DEFAULT_DB_FILE_NAME: &str = "dispatchd.sqlite3";
const DEFAULT_CARRYOVER_LOOKBACK_DAYS: u32 = 7;
const DEFAULT_GITHUB_SYNC_INTERVAL_MINUTES: u32 = 10;

/// The resolved, fully-typed, flat config actually used by the rest of the
/// program. Every field is guaranteed valid.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub thread_creation_time: NaiveTime,
    pub todo_time: NaiveTime,
    pub update_time: NaiveTime,
    /// When the daily meeting actually starts.
    pub meeting_time: NaiveTime,
    /// When the full-detail day progress summary is posted into today's
    /// standup thread - fires on this clock time regardless of whether
    /// everyone has submitted yet, same as the other schedule entries.
    pub day_summary_time: NaiveTime,
    /// How many minutes before `meeting_time` the pre-meeting reminder fires.
    pub meeting_reminder_lead_minutes: u32,
    pub todo_followup_delay_minutes: u32,
    pub update_followup_delay_minutes: u32,
    /// How many days back `/progress add` and the `/team` views look for
    /// unfinished todos to carry forward. `0` disables carry-over.
    pub carryover_lookback_days: u32,
    pub ticker_interval_seconds: u64,
    pub run_on_weekends: bool,
    pub timezone: Tz,
    pub db_path: PathBuf,
    pub discord_guild_id: Option<u64>,
    pub discord_standup_channel_id: Option<u64>,
    /// `None` = the GitHub integration is off (no `[github]` section, or
    /// one without `app_id`/`org`) - every report renders as without it.
    pub github: Option<GithubConfig>,
}

/// `[github]` - the GitHub App dispatchd authenticates as. Neither value
/// is a secret; the App's private key is (see `dispatchd github login`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubConfig {
    pub app_id: u64,
    pub org: String,
    /// How often the ticker pulls new GitHub activity (phase 2).
    pub sync_interval_minutes: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            thread_creation_time: parse_time(DEFAULT_THREAD_CREATION_TIME)
                .expect("default thread_creation_time is valid"),
            todo_time: parse_time(DEFAULT_TODO_TIME).expect("default todo_time is valid"),
            update_time: parse_time(DEFAULT_UPDATE_TIME).expect("default update_time is valid"),
            meeting_time: parse_time(DEFAULT_MEETING_TIME).expect("default meeting_time is valid"),
            day_summary_time: parse_time(DEFAULT_DAY_SUMMARY_TIME)
                .expect("default day_summary_time is valid"),
            meeting_reminder_lead_minutes: DEFAULT_MEETING_REMINDER_LEAD_MINUTES,
            todo_followup_delay_minutes: DEFAULT_TODO_FOLLOWUP_DELAY_MINUTES,
            update_followup_delay_minutes: DEFAULT_UPDATE_FOLLOWUP_DELAY_MINUTES,
            carryover_lookback_days: DEFAULT_CARRYOVER_LOOKBACK_DAYS,
            ticker_interval_seconds: DEFAULT_TICKER_INTERVAL_SECONDS,
            run_on_weekends: DEFAULT_RUN_ON_WEEKENDS,
            timezone: parse_timezone(DEFAULT_TIMEZONE).expect("default timezone is valid"),
            db_path: xdg_default_db_path().expect("default db_path is resolvable"),
            discord_guild_id: None,
            discord_standup_channel_id: None,
            github: None,
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
    #[serde(default)]
    carryover: RawCarryover,
    #[serde(default)]
    github: RawGithub,
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
    day_summary_time: Option<String>,
    meeting_reminder_lead_minutes: Option<u32>,
    ticker_interval_seconds: Option<u64>,
    run_on_weekends: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct RawFollowup {
    todo_delay_minutes: Option<u32>,
    update_delay_minutes: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
struct RawCarryover {
    lookback_days: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
struct RawGithub {
    app_id: Option<u64>,
    org: Option<String>,
    sync_interval_minutes: Option<u32>,
}

/// `app_id` and `org` together switch the integration on; neither = off.
/// Exactly one of them is almost certainly a half-finished edit, so it's an
/// error rather than silently staying off.
fn github_from_raw(raw: RawGithub) -> Result<Option<GithubConfig>> {
    let org = raw.org.map(|o| o.trim().to_string());
    let (app_id, org) = match (raw.app_id, org) {
        (None, None) => return Ok(None),
        (Some(app_id), Some(org)) => (app_id, org),
        (Some(_), None) => anyhow::bail!("github.app_id is set but github.org is not - set both"),
        (None, Some(_)) => anyhow::bail!("github.org is set but github.app_id is not - set both"),
    };
    if org.is_empty() {
        anyhow::bail!("github.org must not be empty");
    }
    let sync_interval_minutes = raw
        .sync_interval_minutes
        .unwrap_or(DEFAULT_GITHUB_SYNC_INTERVAL_MINUTES);
    if sync_interval_minutes == 0 {
        anyhow::bail!("github.sync_interval_minutes must be at least 1");
    }
    Ok(Some(GithubConfig {
        app_id,
        org,
        sync_interval_minutes,
    }))
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

    /// `Config::load` for root-only CLI commands (`github login`,
    /// `status`) that read config the bot normally owns. `sudo` usually
    /// resets `HOME`, so plain XDG lookup would see root's (usually
    /// absent) config instead of the operator's. When this process has no
    /// config file of its own and was started via `sudo`, falls back to
    /// the invoking user's `~/.config/dispatchd/config.toml`. Returns the
    /// file actually used, if any, so the caller can say which it read.
    pub fn load_for_sudo() -> Result<(Config, Option<PathBuf>)> {
        if let Some(path) = config_file_path()? {
            return Ok((Config::load()?, Some(path)));
        }
        let sudo_home = env::var("SUDO_USER").ok().and_then(|user| home_of(&user));
        match sudo_home.map(|home| sudo_user_config_path(&home)) {
            Some(path) if path.exists() => {
                let config = Config::from_raw(read_raw_config(&path)?)?;
                Ok((config, Some(path)))
            }
            _ => Ok((Config::load()?, None)),
        }
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
        let day_summary_time = match raw.schedule.day_summary_time {
            Some(s) => parse_time(&s)
                .with_context(|| format!("invalid schedule.day_summary_time: {s:?}"))?,
            None => defaults.day_summary_time,
        };
        let timezone = match raw.timezone {
            Some(s) => parse_timezone(&s).with_context(|| format!("invalid timezone: {s:?}"))?,
            None => defaults.timezone,
        };
        let db_path = match raw.db_path {
            Some(s) => PathBuf::from(s),
            None => defaults.db_path,
        };
        let github = github_from_raw(raw.github)?;

        Ok(Config {
            thread_creation_time,
            todo_time,
            update_time,
            meeting_time,
            day_summary_time,
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
            carryover_lookback_days: raw
                .carryover
                .lookback_days
                .unwrap_or(defaults.carryover_lookback_days),
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
            github,
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

/// Where `user`'s config lives by default, given their home directory -
/// the XDG default, since their own `XDG_CONFIG_HOME` isn't visible from
/// a `sudo` process.
fn sudo_user_config_path(home: &Path) -> PathBuf {
    home.join(".config").join("dispatchd").join("config.toml")
}

/// `user`'s home directory via `getent passwd` (covers NSS/LDAP accounts,
/// not just `/etc/passwd`). `None` if the lookup fails.
fn home_of(user: &str) -> Option<PathBuf> {
    let output = std::process::Command::new("getent")
        .args(["passwd", user])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    passwd_home(&String::from_utf8_lossy(&output.stdout))
}

/// The home directory (6th field) of a `passwd` line.
fn passwd_home(line: &str) -> Option<PathBuf> {
    line.trim()
        .split(':')
        .nth(5)
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
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
    fn passwd_home_reads_the_sixth_field() {
        assert_eq!(
            passwd_home("pi:x:1000:1000:,,,:/home/pi:/bin/bash\n"),
            Some(PathBuf::from("/home/pi"))
        );
        assert_eq!(passwd_home("broken"), None);
        assert_eq!(passwd_home("nohome:x:1:1:::/bin/sh"), None);
    }

    #[test]
    fn sudo_user_config_path_is_the_xdg_default_under_their_home() {
        assert_eq!(
            sudo_user_config_path(Path::new("/home/pi")),
            PathBuf::from("/home/pi/.config/dispatchd/config.toml")
        );
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
        assert_eq!(config.day_summary_time, defaults.day_summary_time);
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
        assert_eq!(
            config.carryover_lookback_days,
            defaults.carryover_lookback_days
        );
    }

    fn github_from(toml: &str) -> Result<Option<GithubConfig>> {
        let (_dir, path) = write_config(toml);
        Config::from_raw(read_raw_config(&path).unwrap()).map(|c| c.github)
    }

    #[test]
    fn github_is_off_without_a_section() {
        assert_eq!(github_from("").unwrap(), None);
        assert_eq!(github_from("[github]\n").unwrap(), None);
    }

    #[test]
    fn github_is_on_with_app_id_and_org_and_defaults_the_interval() {
        assert_eq!(
            github_from("[github]\napp_id = 123456\norg = \"oxgrad\"\n").unwrap(),
            Some(GithubConfig {
                app_id: 123456,
                org: "oxgrad".into(),
                sync_interval_minutes: 10,
            })
        );
    }

    #[test]
    fn github_sync_interval_is_overridable() {
        let got =
            github_from("[github]\napp_id = 1\norg = \"oxgrad\"\nsync_interval_minutes = 30\n")
                .unwrap()
                .unwrap();
        assert_eq!(got.sync_interval_minutes, 30);
    }

    #[test]
    fn github_rejects_only_one_of_app_id_and_org() {
        let err = github_from("[github]\napp_id = 1\n").unwrap_err();
        assert!(err.to_string().contains("github.org is not"), "{err}");
        let err = github_from("[github]\norg = \"oxgrad\"\n").unwrap_err();
        assert!(err.to_string().contains("github.app_id is not"), "{err}");
    }

    #[test]
    fn github_rejects_an_empty_org_and_a_zero_interval() {
        let err = github_from("[github]\napp_id = 1\norg = \"  \"\n").unwrap_err();
        assert!(err.to_string().contains("must not be empty"), "{err}");
        let err =
            github_from("[github]\napp_id = 1\norg = \"oxgrad\"\nsync_interval_minutes = 0\n")
                .unwrap_err();
        assert!(err.to_string().contains("at least 1"), "{err}");
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
    fn day_summary_time_defaults_to_4pm_and_is_overridable_independently() {
        assert_eq!(
            Config::default().day_summary_time,
            NaiveTime::from_hms_opt(16, 0, 0).unwrap()
        );

        let (_dir, path) = write_config("[schedule]\nday_summary_time = \"18:30\"\n");
        let raw = read_raw_config(&path).unwrap();
        let config = Config::from_raw(raw).unwrap();

        let defaults = Config::default();
        assert_eq!(
            config.day_summary_time,
            NaiveTime::from_hms_opt(18, 30, 0).unwrap()
        );
        assert_eq!(config.meeting_time, defaults.meeting_time);
        assert_eq!(config.update_time, defaults.update_time);
    }

    #[test]
    fn invalid_day_summary_time_is_an_error() {
        let (_dir, path) = write_config("[schedule]\nday_summary_time = \"not-a-time\"\n");
        let raw = read_raw_config(&path).unwrap();
        let err = Config::from_raw(raw).unwrap_err();
        assert!(err.to_string().contains("day_summary_time"), "{err}");
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
            day_summary_time = "18:00"
            meeting_reminder_lead_minutes = 10
            ticker_interval_seconds = 120
            run_on_weekends = true

            [followup]
            todo_delay_minutes = 45
            update_delay_minutes = 20

            [carryover]
            lookback_days = 2
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
        assert_eq!(
            config.day_summary_time,
            NaiveTime::from_hms_opt(18, 0, 0).unwrap()
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
        assert_eq!(config.carryover_lookback_days, 2);
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

    #[test]
    fn carryover_lookback_defaults_to_seven_and_is_overridable() {
        assert_eq!(Config::default().carryover_lookback_days, 7);

        let (_dir, path) = write_config("[carryover]\nlookback_days = 3\n");
        let raw = read_raw_config(&path).unwrap();
        let config = Config::from_raw(raw).unwrap();
        assert_eq!(config.carryover_lookback_days, 3);
    }

    #[test]
    fn carryover_lookback_accepts_zero() {
        let (_dir, path) = write_config("[carryover]\nlookback_days = 0\n");
        let raw = read_raw_config(&path).unwrap();
        let config = Config::from_raw(raw).unwrap();
        assert_eq!(config.carryover_lookback_days, 0);
    }
}

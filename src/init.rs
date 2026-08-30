use std::path::Path;

use anyhow::{Context, Result};

use crate::{config, lock, members};

const CONFIG_TEMPLATE: &str = include_str!("../config.example.toml");
const MEMBERS_TEMPLATE: &str = include_str!("../members.example.toml");

/// Writes `config.toml` and `members.toml` templates to their resolved
/// locations, if they don't already exist. Never overwrites an existing
/// file - safe to re-run.
pub fn run() -> Result<()> {
    let config_path = config::config_target_path()?;

    // Guards the check-then-write below against two `dispatchd init`
    // processes racing each other - without it, both could see
    // config.toml as missing and write it concurrently.
    let _singleton = lock::acquire(&config_path.with_extension("lock"))?;

    create_if_missing(&config_path, "config.toml", CONFIG_TEMPLATE)?;
    create_if_missing(&members::target_path()?, "members.toml", MEMBERS_TEMPLATE)?;
    Ok(())
}

fn create_if_missing(path: &Path, label: &str, template: &str) -> Result<()> {
    if path.exists() {
        println!("{label} already exists at {}, skipping", path.display());
        return Ok(());
    }
    std::fs::write(path, template)
        .with_context(|| format!("failed to write {label} to {}", path.display()))?;
    println!("created {label} at {}", path.display());
    if label == "members.toml" {
        println!("  edit it to add your team's real Discord user IDs, then restart dispatchd");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ENV_LOCK;
    use std::env;

    fn with_scratch_paths<R>(f: impl FnOnce(&Path, &Path) -> R) -> R {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let members_path = dir.path().join("members.toml");
        // SAFETY: held under ENV_LOCK; both vars removed before returning.
        unsafe {
            env::set_var(config::CONFIG_PATH_OVERRIDE_ENV, &config_path);
            env::set_var(members::MEMBERS_PATH_OVERRIDE_ENV, &members_path);
        }
        let result = f(&config_path, &members_path);
        unsafe {
            env::remove_var(config::CONFIG_PATH_OVERRIDE_ENV);
            env::remove_var(members::MEMBERS_PATH_OVERRIDE_ENV);
        }
        result
    }

    #[test]
    fn fresh_init_creates_both_files_with_template_content() {
        with_scratch_paths(|config_path, members_path| {
            run().unwrap();
            assert_eq!(
                std::fs::read_to_string(config_path).unwrap(),
                CONFIG_TEMPLATE
            );
            assert_eq!(
                std::fs::read_to_string(members_path).unwrap(),
                MEMBERS_TEMPLATE
            );
        });
    }

    #[test]
    fn rerunning_init_does_not_overwrite_existing_files() {
        with_scratch_paths(|config_path, members_path| {
            run().unwrap();
            std::fs::write(config_path, "# edited by hand\n").unwrap();
            std::fs::write(members_path, "# edited by hand\n").unwrap();

            run().unwrap();

            assert_eq!(
                std::fs::read_to_string(config_path).unwrap(),
                "# edited by hand\n"
            );
            assert_eq!(
                std::fs::read_to_string(members_path).unwrap(),
                "# edited by hand\n"
            );
        });
    }

    #[test]
    fn fresh_init_config_loads_to_defaults() {
        // Regression test: `init`'s generated config.toml must not silently
        // override the real defaults (it did, once, via a hardcoded
        // example db_path - everything below must ship commented out).
        with_scratch_paths(|_config_path, _members_path| {
            run().unwrap();
            let loaded = config::Config::load().unwrap();
            assert_eq!(loaded, config::Config::default());
        });
    }

    #[test]
    fn fresh_init_members_seeds_nothing() {
        // Regression test: `init`'s generated members.toml must not seed
        // its placeholder rows as real (junk) data.
        with_scratch_paths(|_config_path, _members_path| {
            run().unwrap();
            let conn =
                crate::db::open(&tempfile::tempdir().unwrap().path().join("d.sqlite3")).unwrap();
            let seeded = members::seed(&conn).unwrap();
            assert_eq!(seeded, 0);
        });
    }

    #[test]
    fn only_the_missing_file_is_created() {
        with_scratch_paths(|config_path, members_path| {
            std::fs::write(config_path, "# already here\n").unwrap();
            run().unwrap();

            assert_eq!(
                std::fs::read_to_string(config_path).unwrap(),
                "# already here\n"
            );
            assert_eq!(
                std::fs::read_to_string(members_path).unwrap(),
                MEMBERS_TEMPLATE
            );
        });
    }
}

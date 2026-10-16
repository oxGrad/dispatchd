//! GitHub App integration (see
//! `docs/superpowers/specs/2026-10-15-github-activity-design.md`): `auth`
//! (key parsing, JWT, token refresh rule), `client` (the REST/GraphQL
//! calls + response parsing), `graphql` (the sync's queries and pure
//! parsers), `store` (the `github_*` tables), `sync` (the background sync
//! loop and `github preview`), and the key resolution below. Nothing reads
//! the synced data for reports yet (design phase 3).

pub mod auth;
pub mod client;
pub mod graphql;
pub mod store;
pub mod sync;

use std::env;

/// `systemd-creds` credential name for the App private key, matching
/// `LoadCredentialEncrypted=github_app_key:...` in the unit.
pub const CRED_NAME: &str = "github_app_key";
/// Dev/local fallback outside systemd: a path to the plaintext `.pem`.
pub const KEY_PATH_ENV: &str = "DISPATCHD_GITHUB_APP_KEY_PATH";

/// Resolves the App private key PEM the way the bot does at runtime: the
/// systemd-decrypted credential at `$CREDENTIALS_DIRECTORY/github_app_key`
/// first (see `service::render_unit`), then `DISPATCHD_GITHUB_APP_KEY_PATH`
/// for runs outside systemd. Same shape as `main.rs::discord_token`.
pub fn app_key_pem() -> Option<String> {
    let read_non_empty = |path: std::path::PathBuf| {
        std::fs::read_to_string(path)
            .ok()
            .filter(|contents| !contents.trim().is_empty())
    };
    env::var("CREDENTIALS_DIRECTORY")
        .ok()
        .and_then(|dir| read_non_empty(std::path::Path::new(&dir).join(CRED_NAME)))
        .or_else(|| read_non_empty(env::var(KEY_PATH_ENV).ok()?.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_key_pem_prefers_the_credentials_directory() {
        let _guard = crate::test_support::ENV_LOCK.lock().unwrap();
        let creds = tempfile::tempdir().unwrap();
        std::fs::write(creds.path().join(CRED_NAME), "from-creds\n").unwrap();
        let dev = tempfile::tempdir().unwrap();
        let dev_key = dev.path().join("key.pem");
        std::fs::write(&dev_key, "from-env\n").unwrap();
        unsafe {
            env::set_var("CREDENTIALS_DIRECTORY", creds.path());
            env::set_var(KEY_PATH_ENV, &dev_key);
        }

        let got = app_key_pem();

        unsafe {
            env::remove_var("CREDENTIALS_DIRECTORY");
            env::remove_var(KEY_PATH_ENV);
        }
        assert_eq!(got.as_deref(), Some("from-creds\n"));
    }

    #[test]
    fn app_key_pem_falls_back_to_the_env_path() {
        let _guard = crate::test_support::ENV_LOCK.lock().unwrap();
        let dev = tempfile::tempdir().unwrap();
        let dev_key = dev.path().join("key.pem");
        std::fs::write(&dev_key, "from-env\n").unwrap();
        unsafe {
            env::remove_var("CREDENTIALS_DIRECTORY");
            env::set_var(KEY_PATH_ENV, &dev_key);
        }

        let got = app_key_pem();

        unsafe {
            env::remove_var(KEY_PATH_ENV);
        }
        assert_eq!(got.as_deref(), Some("from-env\n"));
    }

    #[test]
    fn app_key_pem_is_none_when_nothing_is_set() {
        let _guard = crate::test_support::ENV_LOCK.lock().unwrap();
        unsafe {
            env::remove_var("CREDENTIALS_DIRECTORY");
            env::remove_var(KEY_PATH_ENV);
        }
        assert_eq!(app_key_pem(), None);
    }
}

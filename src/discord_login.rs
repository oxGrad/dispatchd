use std::process::Command;

#[cfg(target_os = "linux")]
use std::io::Write as _;
#[cfg(target_os = "linux")]
use std::process::Stdio;

use anyhow::{Context, Result};

/// `dispatchd discord login`: prompts for the bot token, validates it
/// against Discord's API, then encrypts and saves it at rest via
/// `systemd-creds` - dispatchd runs the encryption itself rather than
/// printing a command for the operator to run by hand. `--with-key=host`
/// is used because Raspberry Pi boards (Zero 2 W, 3B, etc.) have no TPM2;
/// this protects an offline copy of the SD card, not root on the live Pi.
///
/// Linux only - the whole flow is built on `systemd-creds`. The
/// `not(target_os = "linux")` stub below mirrors `service::install`.
#[cfg(target_os = "linux")]
pub async fn run() -> Result<()> {
    let version = crate::service::systemd_version()?;
    if version < crate::service::MIN_SYSTEMD_VERSION {
        anyhow::bail!(
            "dispatchd discord login requires systemd >= {} (systemd-creds, for encrypting \
             the token at rest) but found systemd {version} - there is no plaintext fallback. \
             Upgrade the OS (e.g. Raspberry Pi OS Bookworm or newer) and retry.",
            crate::service::MIN_SYSTEMD_VERSION
        );
    }

    let token = rpassword::prompt_password("Discord bot token: ")?;

    let http = serenity::http::Http::new(&token);
    let user = http.get_current_user().await.context(
        "failed to validate the token with Discord - check it's correct and this machine has network access",
    )?;

    std::fs::create_dir_all(crate::service::ENV_DIR).with_context(|| {
        format!(
            "failed to create {} (are you root? try: sudo dispatchd discord login)",
            crate::service::ENV_DIR
        )
    })?;
    encrypt_token(&token, crate::service::CRED_PATH)?;
    drop(token);

    println!("Logged in as {} ({}).", user.name, user.id);
    println!(
        "Token encrypted and saved to {}.",
        crate::service::CRED_PATH
    );
    println!(
        "Run `sudo systemctl restart dispatchd` (or `start`, if it isn't running yet) to pick it up."
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub async fn run() -> Result<()> {
    anyhow::bail!("`dispatchd discord login` is only supported on Linux (systemd-creds)")
}

/// Pipes `token` into `systemd-creds encrypt ... - cred_path`, so the
/// plaintext token only ever exists in memory and in the pipe to that
/// child process, never as an intermediate file. Overwrites `cred_path` if
/// it already exists - re-running `discord login` is how you rotate the
/// token.
#[cfg(target_os = "linux")]
fn encrypt_token(token: &str, cred_path: &str) -> Result<()> {
    let mut child = Command::new("systemd-creds")
        .args(["encrypt", "--name=discord_token", "--with-key=host", "-"])
        .arg(cred_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context(
            "failed to run `systemd-creds encrypt` (are you root? try: sudo dispatchd discord login)",
        )?;

    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(token.as_bytes())
        .context("failed to write the token to systemd-creds' stdin")?;

    let status = child
        .wait()
        .context("failed to wait for `systemd-creds encrypt` to finish")?;
    if !status.success() {
        anyhow::bail!("`systemd-creds encrypt` failed ({status})");
    }
    Ok(())
}

/// `dispatchd discord logout`: removes the encrypted credential written by
/// `discord login`. Requires root, since it lives under `/etc`. Idempotent:
/// a missing credential is not an error, matching `service::install`'s
/// "missing is a valid state" convention elsewhere in this project.
pub fn logout() -> Result<()> {
    logout_at(crate::service::CRED_PATH)
}

fn logout_at(cred_path: &str) -> Result<()> {
    if !std::path::Path::new(cred_path).exists() {
        println!("no encrypted Discord token found at {cred_path} - nothing to do.");
        return Ok(());
    }
    std::fs::remove_file(cred_path).with_context(|| {
        format!("failed to remove {cred_path} (are you root? try: sudo dispatchd discord logout)")
    })?;
    println!("removed {cred_path}.");
    println!(
        "dispatchd.service (if running) keeps the old token in memory until restarted - run \
         `sudo systemctl stop dispatchd` if you want to disconnect it now."
    );
    Ok(())
}

/// Decrypts the stored credential directly, for `dispatchd status`'s
/// Discord ping. Unlike the long-running bot (started by systemd itself,
/// which decrypts `LoadCredentialEncrypted=` automatically into
/// `$CREDENTIALS_DIRECTORY` - see `main.rs::discord_token`), `dispatchd
/// status` is a one-off interactive invocation with no such directory, so
/// it decrypts the credential itself. Requires root (the `--with-key=host`
/// key is root-only); returns `None` rather than erroring on any failure
/// so `status` can just report "no token" instead of aborting.
pub fn decrypt_cred_file(cred_path: &str) -> Option<String> {
    if !std::path::Path::new(cred_path).exists() {
        return None;
    }
    let output = Command::new("systemd-creds")
        .args(["decrypt", cred_path, "-"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8(output.stdout).ok()?;
    let trimmed = token.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// `dispatchd status`'s Discord half: resolves a token (the same way the
/// bot itself does at startup, or by decrypting the credential file
/// directly - see `decrypt_cred_file`), then pings Discord's API and
/// reports round-trip latency.
pub async fn ping() {
    println!("discord:");
    let token =
        match crate::discord_token().or_else(|| decrypt_cred_file(crate::service::CRED_PATH)) {
            Some(token) => token,
            None => {
                println!("  token:                not found - run: sudo dispatchd discord login");
                return;
            }
        };

    let start = std::time::Instant::now();
    let http = serenity::http::Http::new(&token);
    match http.get_current_user().await {
        Ok(user) => println!(
            "  ping:                 ok - logged in as {} ({}), {}ms",
            user.name,
            user.id,
            start.elapsed().as_millis()
        ),
        Err(e) => println!("  ping:                 failed - {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decrypt_cred_file_returns_none_for_a_missing_file() {
        assert_eq!(decrypt_cred_file("/nonexistent/discord_token.cred"), None);
    }

    #[test]
    fn logout_at_removes_an_existing_credential() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("discord_token.cred");
        std::fs::write(&path, "encrypted-stuff").unwrap();

        logout_at(path.to_str().unwrap()).unwrap();

        assert!(!path.exists());
    }

    #[test]
    fn logout_at_on_a_missing_credential_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("discord_token.cred");

        logout_at(path.to_str().unwrap()).unwrap();
    }
}

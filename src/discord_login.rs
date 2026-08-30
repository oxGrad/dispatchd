use anyhow::{Context, Result};

use crate::token_store::TokenStore;

/// Persists a Discord bot token to the token store. Split out from the
/// interactive prompt + live validation call in `run` below so this - the
/// part with real logic worth testing - doesn't need a live Discord
/// connection to test.
pub fn persist_token(token: &str, store: &dyn TokenStore) -> Result<()> {
    store.save(token)
}

/// `dispatchd discord login`: prompts for the bot token, validates it
/// against Discord's API, and saves it to the OS keyring. Mirrors
/// HiveMind's `matrix login` UX (prompt, validate live, persist to the OS
/// keyring) rather than a config file or an operator-encrypted secret.
pub async fn run() -> Result<()> {
    let token = rpassword::prompt_password("Discord bot token: ")?;

    let http = serenity::http::Http::new(&token);
    let user = http.get_current_user().await.context(
        "failed to validate the token with Discord - check it's correct and this machine has network access",
    )?;

    let store = crate::token_store::KeyringTokenStore;
    persist_token(&token, &store)?;
    drop(token);

    println!("Logged in as {} ({}).", user.name, user.id);
    println!("Token saved to the OS keyring. Run `dispatchd` to start the bot.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_store::FakeTokenStore;

    #[test]
    fn persists_token_to_the_store() {
        let store = FakeTokenStore::new();
        persist_token("test-token", &store).unwrap();
        assert_eq!(store.load().unwrap(), Some("test-token".to_string()));
    }
}

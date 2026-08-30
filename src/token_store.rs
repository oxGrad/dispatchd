use anyhow::Result;

/// Keyring service name, namespacing dispatchd's entry from any other
/// application sharing the same OS keyring.
const SERVICE: &str = "dispatchd-discord";
/// Fixed keyring username. Unlike HiveMind's per-account `SessionStore`
/// (keyed by Matrix user_id, known ahead of time from config.toml), the
/// Discord bot token is dispatchd's only credential and isn't tied to a
/// pre-known identity string - there's exactly one per install.
const TOKEN_KEY: &str = "bot-token";

pub trait TokenStore: Send + Sync {
    fn save(&self, token: &str) -> Result<()>;
    fn load(&self) -> Result<Option<String>>;
}

pub struct KeyringTokenStore;

impl TokenStore for KeyringTokenStore {
    fn save(&self, token: &str) -> Result<()> {
        let entry = keyring::Entry::new(SERVICE, TOKEN_KEY)?;
        entry.set_password(token)?;
        Ok(())
    }

    fn load(&self) -> Result<Option<String>> {
        let entry = keyring::Entry::new(SERVICE, TOKEN_KEY)?;
        match entry.get_password() {
            Ok(pw) => Ok(Some(pw)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
pub struct FakeTokenStore(std::sync::Mutex<Option<String>>);

#[cfg(test)]
impl Default for FakeTokenStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl FakeTokenStore {
    pub fn new() -> Self {
        FakeTokenStore(std::sync::Mutex::new(None))
    }
}

#[cfg(test)]
impl TokenStore for FakeTokenStore {
    fn save(&self, token: &str) -> Result<()> {
        *self.0.lock().unwrap() = Some(token.to_string());
        Ok(())
    }

    fn load(&self) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_on_empty_store_returns_none() {
        let store = FakeTokenStore::new();
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn save_then_load_round_trips() {
        let store = FakeTokenStore::new();
        store.save("abc.def.ghi").unwrap();
        assert_eq!(store.load().unwrap(), Some("abc.def.ghi".to_string()));
    }

    #[test]
    fn save_overwrites_the_previous_token() {
        let store = FakeTokenStore::new();
        store.save("old-token").unwrap();
        store.save("new-token").unwrap();
        assert_eq!(store.load().unwrap(), Some("new-token".to_string()));
    }
}

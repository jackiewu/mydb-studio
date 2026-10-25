//! The OS keychain as the store's password vault.

use mydb_core::secrets::SecretVault;

/// The keychain on macOS and Windows. Elsewhere passwords stay in the app
/// database, as there is no keychain this app can count on.
pub fn vault() -> Option<Box<dyn SecretVault>> {
    #[cfg(any(target_os = "macos", windows))]
    {
        Some(Box::new(Keychain))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        None
    }
}

#[cfg(any(target_os = "macos", windows))]
struct Keychain;

#[cfg(any(target_os = "macos", windows))]
impl Keychain {
    fn entry() -> Result<keyring::Entry, String> {
        keyring::Entry::new("MyDB Studio", "connection passwords").map_err(|e| e.to_string())
    }
}

#[cfg(any(target_os = "macos", windows))]
impl SecretVault for Keychain {
    fn load(&self) -> Result<Option<String>, String> {
        match Self::entry()?.get_password() {
            Ok(data) => Ok(Some(data)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn save(&self, data: &str) -> Result<(), String> {
        Self::entry()?.set_password(data).map_err(|e| e.to_string())
    }
}

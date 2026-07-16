use keyring::Entry;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("keyring error: {0}")]
    Keyring(#[from] keyring::Error),
}

const SERVICE: &str = "any-converter";

pub fn key_ref(provider_name: &str) -> String {
    format!("keychain:{SERVICE}:{provider_name}")
}

pub fn set_provider_key(provider_name: &str, api_key: &str) -> Result<String, SecretError> {
    let entry = Entry::new(SERVICE, provider_name)?;
    entry.set_password(api_key)?;
    Ok(key_ref(provider_name))
}

pub fn get_provider_key(provider_name: &str) -> Result<String, SecretError> {
    let entry = Entry::new(SERVICE, provider_name)?;
    Ok(entry.get_password()?)
}

pub fn delete_provider_key(provider_name: &str) -> Result<(), SecretError> {
    let entry = Entry::new(SERVICE, provider_name)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

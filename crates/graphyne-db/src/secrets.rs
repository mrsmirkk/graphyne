//! Database passwords in the operating system's credential store (the Windows Credential
//! Manager), never in Graphyne's files. Each connection has its own key.

const SERVICE: &str = "Graphyne";

/// A fresh, unique key for a connection's password.
pub fn new_key(id: &str) -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    format!("database/{id}/{nanos:x}")
}

fn entry(key: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, key).map_err(|e| format!("the credential store is not available: {e}"))
}

/// Remember `password` under `key` (an empty password removes it).
pub fn set_password(key: &str, password: &str) -> Result<(), String> {
    if password.is_empty() {
        return delete_password(key);
    }
    entry(key)?.set_password(password).map_err(|e| format!("could not save the password: {e}"))
}

/// The password saved under `key`, if any.
pub fn get_password(key: &str) -> Result<Option<String>, String> {
    match entry(key)?.get_password() {
        Ok(p) => Ok(Some(p)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("could not read the password: {e}")),
    }
}

pub fn delete_password(key: &str) -> Result<(), String> {
    match entry(key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("could not remove the password: {e}")),
    }
}

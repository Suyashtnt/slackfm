use age::secrecy::Secret;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Serialize, Deserialize)]
pub struct UserData {
    lastfm_username: String,
    slack_token: String,
}

impl UserData {
    pub fn new(lastfm_username: String, slack_token: String) -> Self {
        UserData {
            lastfm_username,
            slack_token,
        }
    }

    pub fn update_lastfm_username(&mut self, lastfm_username: String) {
        self.lastfm_username = lastfm_username;
    }

    pub fn slack_token(&self) -> &str {
        &self.slack_token
    }

    pub fn lastfm_username(&self) -> &str {
        &self.lastfm_username
    }
}

#[derive(Serialize, Deserialize)]
pub struct Db(HashMap<String, Arc<Mutex<UserData>>>);

impl Db {
    pub fn new() -> Self {
        Db(HashMap::new())
    }

    /// Create a new Db instance from an encrypted file
    pub fn from_encrypted_file(file_path: &Path, key: &str) -> Option<Self> {
        if !file_path.exists() {
            return None;
        }

        let file_reader = std::fs::File::open(file_path).ok()?;

        let decrypted: Db = {
            let decryptor = match age::Decryptor::new(&file_reader).ok()? {
                age::Decryptor::Passphrase(d) => d,
                _ => unreachable!(),
            };

            let mut reader = decryptor.decrypt(&Secret::new(key.to_owned()), None).ok()?;
            serde_json::from_reader(&mut reader).ok()?
        };

        Some(decrypted)
    }

    pub fn to_encrypted_file(
        &self,
        file_path: &Path,
        key: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let encrypted = {
            let encryptor = age::Encryptor::with_user_passphrase(Secret::new(key.to_owned()));

            let mut encrypted = vec![];
            let mut writer = encryptor.wrap_output(&mut encrypted)?;
            serde_json::to_writer(&mut writer, self)?;
            writer.finish()?;

            encrypted
        };

        std::fs::write(file_path, encrypted)?;

        Ok(())
    }

    pub fn user(&self, username: &str) -> Option<Arc<Mutex<UserData>>> {
        self.0.get(username).cloned()
    }

    pub fn users(&self) -> impl Iterator<Item = (&String, Arc<Mutex<UserData>>)> {
        self.0.iter().map(|(k, v)| (k, v.clone()))
    }
}

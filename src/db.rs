use age::secrecy::Secret;
use oauth2::CsrfToken;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Serialize, Deserialize)]
pub struct UserData {
    lastfm_username: String,
    slack_token: SlackToken,
}

#[derive(Serialize, Deserialize)]
pub enum SlackToken {
    Oauth(String),
    // we might be waiting for the user to authorize the app
    Csrf(CsrfToken),
}

impl UserData {
    pub fn new(lastfm_username: String, csrf: CsrfToken) -> Self {
        UserData {
            lastfm_username,
            slack_token: SlackToken::Csrf(csrf),
        }
    }

    pub fn update_lastfm_username(&mut self, lastfm_username: String) {
        self.lastfm_username = lastfm_username;
    }

    pub fn slack_token(&self) -> Option<&str> {
        match &self.slack_token {
            SlackToken::Oauth(token) => Some(token),
            _ => None,
        }
    }

    pub fn csrf_token(&self) -> Option<&CsrfToken> {
        match &self.slack_token {
            SlackToken::Csrf(token) => Some(token),
            _ => None,
        }
    }

    pub fn lastfm_username(&self) -> &str {
        &self.lastfm_username
    }

    pub fn promote_token(&mut self, token: String) {
        self.slack_token = SlackToken::Oauth(token);
    }
}

pub struct Db {
    db: HashMap<String, Arc<Mutex<UserData>>>,
    location: PathBuf,
    key: String,
}

impl Db {
    pub fn new(file_path: PathBuf, key: String) -> Self {
        Db {
            db: HashMap::new(),
            location: file_path,
            key,
        }
    }

    /// Create a new Db instance from an encrypted file
    pub fn from_encrypted_file(
        file_path: PathBuf,
        key: String,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if !file_path.exists() {
            return Ok(Self::new(file_path, key));
        }

        let file_reader = std::fs::File::open(&file_path)?;

        let db: HashMap<String, Arc<Mutex<UserData>>> = {
            let decryptor = match age::Decryptor::new(&file_reader)? {
                age::Decryptor::Passphrase(d) => d,
                _ => unreachable!(),
            };

            let mut reader = decryptor.decrypt(&Secret::new(key.to_owned()), None)?;
            serde_json::from_reader(&mut reader)?
        };

        Ok(Self {
            db,
            location: file_path,
            key,
        })
    }

    pub fn to_encrypted_file(&self) -> Result<(), Box<dyn std::error::Error>> {
        let encrypted = {
            let encryptor = age::Encryptor::with_user_passphrase(Secret::new(self.key.clone()));

            let mut encrypted = vec![];
            let mut writer = encryptor.wrap_output(&mut encrypted)?;
            serde_json::to_writer(&mut writer, &self.db)?;
            writer.finish()?;

            encrypted
        };

        std::fs::write(&self.location, encrypted)?;

        Ok(())
    }

    pub fn user(&self, username: &str) -> Option<Arc<Mutex<UserData>>> {
        self.db.get(username).cloned()
    }

    pub fn users(&self) -> impl Iterator<Item = (&String, Arc<Mutex<UserData>>)> {
        self.db.iter().map(|(k, v)| (k, v.clone()))
    }

    pub fn add_user(&mut self, username: String, data: UserData) {
        self.db.insert(username, Arc::new(Mutex::new(data)));
        self.to_encrypted_file().unwrap()
    }

    pub fn user_with_csrf(&self, state: &String) -> Option<Arc<Mutex<UserData>>> {
        self.db
            .iter()
            .find(|(_, user)| {
                user.lock().unwrap().csrf_token().map(CsrfToken::secret) == Some(state)
            })
            .map(|(_, user)| user.clone())
    }
}

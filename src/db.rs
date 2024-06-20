use age::secrecy::Secret;
use error_stack::{Result, ResultExt};
use futures::Future;
use oauth2::CsrfToken;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    error::Error,
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tracing::debug;

#[derive(Serialize, Deserialize, Debug)]
pub struct UserData {
    lastfm_username: String,
    slack_token: SlackToken,
}

#[derive(Serialize, Deserialize, Debug)]
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

#[derive(Debug)]
pub enum DbError {
    EncryptionError,
    IoError,
    SerdeError,
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::EncryptionError => f.write_str("Error encrypting or decrypting the database"),
            DbError::IoError => f.write_str("Error reading or writing the database file"),
            DbError::SerdeError => f.write_str("Error serializing or deserializing the database"),
        }
    }
}

impl Error for DbError {}

impl Db {
    pub fn new(file_path: PathBuf, key: String) -> Self {
        Db {
            db: HashMap::new(),
            location: file_path,
            key,
        }
    }

    /// Gives you full access to the inner db HashMap, but you have to return an updated version
    ///
    /// This is used as a cursed hack to avoid having to clone the entire db when doing bulk updates
    pub async fn map_db<F>(
        &mut self,
        f: impl FnOnce(HashMap<String, Arc<Mutex<UserData>>>) -> F,
    ) -> Result<(), DbError>
    where
        F: Future<Output = HashMap<String, Arc<Mutex<UserData>>>>,
    {
        let db = std::mem::replace(&mut self.db, HashMap::new());
        let final_db = f(db).await;
        self.db = final_db;

        self.to_encrypted_file()
    }

    /// Create a new Db instance from an encrypted file
    #[tracing::instrument(skip(key))]
    pub fn from_encrypted_file(file_path: PathBuf, key: String) -> Result<Self, DbError> {
        if !file_path.exists() {
            return Ok(Self::new(file_path, key));
        }

        let file_reader = std::fs::File::open(&file_path)
            .attach_printable("Couldn't open database file")
            .change_context(DbError::IoError)?;

        let db: HashMap<String, Arc<Mutex<UserData>>> = {
            let decryptor = match age::Decryptor::new(&file_reader)
                .attach_printable("Couldn't create database decryptor")
                .change_context(DbError::EncryptionError)?
            {
                age::Decryptor::Passphrase(d) => d,
                _ => unreachable!(),
            };

            let mut reader = decryptor
                .decrypt(&Secret::new(key.to_owned()), None)
                .attach_printable("Couldn't decrypt database")
                .change_context(DbError::EncryptionError)?;

            serde_json::from_reader(&mut reader)
                .attach_printable("Couldn't deserialize database")
                .change_context(DbError::SerdeError)?
        };

        debug!("Loaded database from file: {:?}", db);

        Ok(Self {
            db,
            location: file_path,
            key,
        })
    }

    #[tracing::instrument(skip(self))]
    pub fn to_encrypted_file(&self) -> Result<(), DbError> {
        let encrypted = {
            let encryptor = age::Encryptor::with_user_passphrase(Secret::new(self.key.clone()));

            let mut encrypted = vec![];
            let mut writer = encryptor
                .wrap_output(&mut encrypted)
                .attach_printable("Couldn't create database encryptor")
                .change_context(DbError::EncryptionError)?;

            serde_json::to_writer(&mut writer, &self.db)
                .attach_printable("Couldn't serialize database")
                .change_context(DbError::SerdeError)?;

            writer
                .finish()
                .attach_printable("Couldn't finish encrypting database")
                .change_context(DbError::EncryptionError)?;

            encrypted
        };

        std::fs::write(&self.location, encrypted)
            .attach_printable("Couldn't write encrypted database to file")
            .change_context(DbError::IoError)?;

        Ok(())
    }

    pub fn user(&self, username: &str) -> Option<Arc<Mutex<UserData>>> {
        self.db.get(username).cloned()
    }

    pub fn users(&self) -> impl Iterator<Item = (&String, Arc<Mutex<UserData>>)> {
        self.db.iter().map(|(k, v)| (k, v.clone()))
    }

    pub fn add_user(&mut self, username: String, data: UserData) -> Result<(), DbError> {
        self.db.insert(username, Arc::new(Mutex::new(data)));
        self.to_encrypted_file()
    }

    pub fn remove_user(&mut self, username: &str) -> Result<Option<Arc<Mutex<UserData>>>, DbError> {
        let user = self.db.remove(username);
        self.to_encrypted_file()?;
        Ok(user)
    }

    pub fn user_with_csrf(&self, state: &String) -> Option<Arc<Mutex<UserData>>> {
        self.db
            .iter()
            .find(|(_, user)| {
                user.lock()
                    .is_ok_and(|user| user.csrf_token().map(CsrfToken::secret) == Some(state))
            })
            .map(|(_, user)| user.clone())
    }
}

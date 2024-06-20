use std::{
    error::Error,
    fmt::{self, Debug},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use error_stack::{Result, ResultExt};
use slack_morphism::prelude::*;
use tracing::debug;

pub struct Client {
    client: Arc<SlackClient<SlackClientHyperConnector<SlackHyperHttpsConnector>>>,
    token: SlackApiToken,
}

#[derive(Debug)]
pub enum SlackError {
    ClientError,
    IoError,
}

impl fmt::Display for SlackError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::ClientError => f.write_str("Slack client error"),
            Self::IoError => f.write_str("IO error"),
        }
    }
}
impl Error for SlackError {}

impl Client {
    #[tracing::instrument]
    pub fn new(
        token: impl Into<SlackApiTokenValue> + Debug,
        team_id: impl Into<SlackTeamId> + Debug,
    ) -> Result<Self, SlackError> {
        debug!("Creating slack client");
        let client = SlackClient::new(
            SlackClientHyperConnector::new()
                .attach_printable("Failed to create HTTP connector for slack")
                .change_context(SlackError::IoError)?
                .with_rate_control(SlackApiRateControlConfig::new()),
        );
        let token: SlackApiToken = SlackApiToken::new(token.into()).with_team_id(team_id.into());
        debug!("Slack client created");

        Ok(Self {
            client: client.into(),
            token,
        })
    }

    #[tracing::instrument(skip(client))]
    pub fn from_client(
        client: Arc<SlackClient<SlackClientHyperConnector<SlackHyperHttpsConnector>>>,
        token: impl Into<SlackApiTokenValue> + Debug,
        team_id: impl Into<SlackTeamId> + Debug,
    ) -> Self {
        Self {
            client,
            token: SlackApiToken::new(token.into()).with_team_id(team_id.into()),
        }
    }

    pub fn client(&self) -> &SlackClient<SlackClientHyperConnector<SlackHyperHttpsConnector>> {
        &self.client
    }

    #[tracing::instrument(skip(self))]
    pub async fn update_user_status(
        &self,
        user_id: SlackUserId,
        status_text: Option<impl Into<String> + Debug>,
        status_emoji: Option<impl Into<SlackEmoji> + Debug>,
        status_duration: Option<DateTime<Utc>>,
    ) -> Result<SlackUserProfile, SlackError> {
        let session = self.client.open_session(&self.token);

        let user_request = SlackApiUsersProfileGetRequest::new().with_user(user_id);

        let user = session
            .users_profile_get(&user_request)
            .await
            .attach_printable("Failed to get user profile")
            .change_context(SlackError::ClientError)?;
        debug!("User profile: {:?}", user);

        let user_update_request = SlackApiUsersProfileSetRequest::new(
            user.profile
                .opt_status_emoji(status_emoji.map(Into::into))
                .opt_status_text(status_text.map(Into::into))
                .opt_status_expiration(
                    status_duration.map(|duration| SlackDateTime::new(duration)),
                ),
        );

        debug!("Updating user profile: {:?}", user_update_request);

        let updated = session
            .users_profile_set(&user_update_request)
            .await
            .attach_printable("Failed to update user profile")
            .change_context(SlackError::ClientError)?;

        debug!("Updated user profile to {:?}", updated.profile);

        Ok(updated.profile)
    }
}

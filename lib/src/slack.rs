use std::io;

use chrono::{DateTime, Utc};
use errors::SlackClientError;
use slack_morphism::prelude::*;

pub struct Slack {
    client: SlackClient<SlackClientHyperConnector<SlackHyperHttpsConnector>>,
    token: SlackApiToken,
}

impl Slack {
    pub fn new(
        token: impl Into<SlackApiTokenValue>,
        team_id: impl Into<SlackTeamId>,
    ) -> io::Result<Self> {
        let client = SlackClient::new(
            SlackClientHyperConnector::new()?.with_rate_control(SlackApiRateControlConfig::new()),
        );
        let token: SlackApiToken = SlackApiToken::new(token.into()).with_team_id(team_id.into());

        Ok(Self { client, token })
    }

    pub fn from_client(
        client: SlackClient<SlackClientHyperConnector<SlackHyperHttpsConnector>>,
        token: impl Into<SlackApiToken>,
        team_id: impl Into<SlackTeamId>,
    ) -> Self {
        Self {
            client,
            token: token.into().with_team_id(team_id.into()),
        }
    }

    pub fn client(&self) -> &SlackClient<SlackClientHyperConnector<SlackHyperHttpsConnector>> {
        &self.client
    }

    pub async fn update_user_status(
        &self,
        username: String,
        status_text: Option<impl Into<String>>,
        status_emoji: Option<impl Into<SlackEmoji>>,
        status_duration: Option<DateTime<Utc>>,
    ) -> Result<SlackUserProfile, SlackClientError> {
        let session = self.client.open_session(&self.token);

        let user_request =
            SlackApiUsersProfileGetRequest::new().with_user(SlackUserId::new(username));

        let user = session.users_profile_get(&user_request).await?;

        let user_update_request = SlackApiUsersProfileSetRequest::new(
            user.profile
                .opt_status_emoji(status_emoji.map(Into::into))
                .opt_status_text(status_text.map(Into::into))
                .opt_status_expiration(
                    status_duration.map(|duration| SlackDateTime::new(duration)),
                ),
        );

        let updated = session.users_profile_set(&user_update_request).await?;

        Ok(updated.profile)
    }
}
use oauth2::{AuthUrl, ClientId, ClientSecret, RedirectUrl, TokenUrl};
use serde::{Deserialize, Serialize};

use crate::env;

#[derive(Serialize, Deserialize, Debug)]
pub struct SlackAuthedUser {
    pub id: String,
    pub scope: String,
    pub access_token: String,
    pub token_type: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SlackTokenFields {
    pub authed_user: SlackAuthedUser,
}
impl oauth2::ExtraTokenFields for SlackTokenFields {}

pub type SlackOauthClient = oauth2::Client<
    oauth2::StandardErrorResponse<oauth2::basic::BasicErrorResponseType>,
    oauth2::StandardTokenResponse<SlackTokenFields, oauth2::basic::BasicTokenType>,
    oauth2::basic::BasicTokenType,
    oauth2::basic::BasicTokenIntrospectionResponse,
    oauth2::StandardRevocableToken,
    oauth2::basic::BasicRevocationErrorResponse,
>;

pub fn create_oauth_client() -> SlackOauthClient {
    SlackOauthClient::new(
        ClientId::new(env::slack_client_id()),
        Some(ClientSecret::new(env::slack_client_secret())),
        AuthUrl::new("https://slack.com/oauth/v2/authorize".to_owned()).unwrap(),
        Some(TokenUrl::new("https://slack.com/api/oauth.v2.access".to_owned()).unwrap()),
    )
    .set_redirect_uri(RedirectUrl::new("https://slackfm.wobbl.in/auth".to_owned()).unwrap())
}

#[derive(Deserialize)]
pub struct OauthCode {
    pub code: String,
    pub state: String,
}

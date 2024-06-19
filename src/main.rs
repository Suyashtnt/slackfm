mod db;

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::Extension;
use db::{Db, UserData};
use dotenvy::dotenv;
use futures::{pin_mut, StreamExt};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::Response;
use oauth2::reqwest::async_http_client;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, RedirectUrl, TokenUrl,
};
use serde::{Deserialize, Serialize};
use slack_morphism::prelude::*;
use slackfm::{lastfm, slack};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

mod env {
    use menv::require_envs;

    require_envs! {
        (assert_env_vars, any_set, gen_help);

        lastfm_key, "LASTFM_API_KEY", String,
        "Please set your last.fm API key in the environment variable LASTFM_API_KEY";

        slack_team_id, "SLACK_TEAM_ID", String,
        "Please set your slack team id in the environment variable SLACK_TEAM_ID";

        slack_client_id, "SLACK_CLIENT_ID", String,
        "Please set your slack client id in the environment variable SLACK_CLIENT_ID";

        slack_client_secret, "SLACK_CLIENT_SECRET", String,
        "Please set your slack client secret in the environment variable SLACK_CLIENT_SECRET";

        slack_signing_secret, "SLACK_SIGNING_SECRET", String,
        "Please set your slack signing secret in the environment variable SLACK_SIGNING_SECRET";
    }
}

#[tokio::main]
async fn main() {
    dotenv().expect(".env file not found");

    if env::any_set() {
        env::assert_env_vars();

        run_server().await.unwrap();
    } else {
        println!("# Environment Variables Help\n{}", env::gen_help());
        return;
    }
}

fn error_handler(
    err: Box<dyn std::error::Error + Send + Sync>,
    _client: Arc<SlackHyperClient>,
    _states: SlackClientEventsUserState,
) -> HttpStatusCode {
    eprintln!("{:#?}", err);

    HttpStatusCode::BAD_REQUEST
}

async fn test_welcome_installed() -> String {
    "Welcome".to_string()
}

async fn test_cancelled_install() -> String {
    "Cancelled".to_string()
}

async fn test_error_install() -> String {
    "Error while installing".to_string()
}

async fn test_push_event(
    Extension(_environment): Extension<Arc<SlackHyperListenerEnvironment>>,
    Extension(event): Extension<SlackPushEvent>,
) -> Response<BoxBody<Bytes, Infallible>> {
    println!("Received push event: {:?}", event);

    match event {
        SlackPushEvent::UrlVerification(url_ver) => {
            Response::new(Full::new(url_ver.challenge.into()).boxed())
        }
        _ => Response::new(Empty::new().boxed()),
    }
}

async fn command_event(
    Extension(_environment): Extension<Arc<SlackHyperListenerEnvironment>>,
    Extension(event): Extension<SlackCommandEvent>,
    State(state): State<AppState>,
) -> axum::Json<SlackCommandEventResponse> {
    match &*event.command.0 {
        "/connect" => {
            println!("Received connect command");

            let Some(lastfm_username) = event.text.and_then(|text| {
                if text.is_empty() {
                    None
                } else {
                    Some(
                        text.split_once(" ")
                            .map(|split| split.0.to_owned())
                            .unwrap_or_else(|| text.to_owned()),
                    )
                }
            }) else {
                return axum::Json(SlackCommandEventResponse::new(
                    SlackMessageContent::new()
                        .with_text("No username found. Please give one".into()),
                ));
            };

            let mut db = state.db.lock().await;

            let user = db.user(&event.user_id.0);

            if let Some(user) = user.filter(|user| user.lock().unwrap().slack_token().is_some()) {
                let mut user = user.lock().unwrap();
                // update their last.fm username
                user.update_lastfm_username(lastfm_username);
                drop(user);
                db.to_encrypted_file().unwrap();

                axum::Json(SlackCommandEventResponse::new(
                    SlackMessageContent::new().with_text("Updated Last.fm username".into()),
                ))
            } else {
                let oauth_client = create_oauth_client();

                // note: we aren't doing PKCE since this is only ran on a trusted server

                let (auth_url, csrf_token) = oauth_client
                    .authorize_url(CsrfToken::new_random)
                    .add_extra_param("scope", "commands")
                    .add_extra_param("user_scope", "users.profile:read,users.profile:write")
                    .url();

                db.add_user(event.user_id.0, UserData::new(lastfm_username, csrf_token));

                // send an oauth link
                axum::Json(
                    SlackCommandEventResponse::new(SlackMessageContent::new().with_text(format!(
                        "Please visit {} to allow SlackFM to access and modify your profile/status",
                        auth_url
                    )))
                    .with_response_type(SlackMessageResponseType::Ephemeral),
                )
            }
        }
        _ => {
            println!("Received unknown command");
            axum::Json(SlackCommandEventResponse::new(
                SlackMessageContent::new().with_text("Received unknown command".into()),
            ))
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct SlackTokenFields {
    pub authed_user: SlackAuthedUser,
}
impl oauth2::ExtraTokenFields for SlackTokenFields {}

#[derive(Serialize, Deserialize, Debug)]
struct SlackAuthedUser {
    pub id: String,
    pub scope: String,
    pub access_token: String,
    pub token_type: String,
}

pub type SlackOauthClient = oauth2::Client<
    oauth2::StandardErrorResponse<oauth2::basic::BasicErrorResponseType>,
    oauth2::StandardTokenResponse<SlackTokenFields, oauth2::basic::BasicTokenType>,
    oauth2::basic::BasicTokenType,
    oauth2::basic::BasicTokenIntrospectionResponse,
    oauth2::StandardRevocableToken,
    oauth2::basic::BasicRevocationErrorResponse,
>;

fn create_oauth_client() -> SlackOauthClient {
    SlackOauthClient::new(
        ClientId::new(env::slack_client_id()),
        Some(ClientSecret::new(env::slack_client_secret())),
        AuthUrl::new("https://slack.com/oauth/v2/authorize".to_owned()).unwrap(),
        Some(TokenUrl::new("https://slack.com/api/oauth.v2.access".to_owned()).unwrap()),
    )
    .set_redirect_uri(RedirectUrl::new("https://slackfm.wobbl.in/auth".to_owned()).unwrap())
}

#[derive(Deserialize)]
struct OauthCode {
    code: String,
    state: String,
}

async fn oauth_handler(
    Query(code): Query<OauthCode>,
    State(state): State<AppState>,
) -> &'static str {
    let db = state.db.lock().await;
    // Retrieve the csrf token and pkce verifier
    let Some(user_arc) = db.user_with_csrf(&code.state) else {
        return "CSRF couldn't be linked to a user. Theres a middleman at play or I didn't save properly";
    };

    let client = create_oauth_client();

    let response = client
        .exchange_code(AuthorizationCode::new(code.code))
        .request_async(async_http_client)
        .await
        .unwrap();

    let user_token = response.extra_fields().authed_user.access_token.clone();
    let user_id = response.extra_fields().authed_user.id.clone();

    let mut user = user_arc.lock().unwrap();
    user.promote_token(user_token);

    drop(user);

    db.to_encrypted_file().unwrap();
    tokio::task::spawn(update_user_data(
        state.slack_client.clone(),
        state.lastfm_client.clone(),
        user_id.into(),
        user_arc,
    ));

    "Authenticated!"
}

#[derive(Clone)]
struct AppState {
    db: Arc<Mutex<Db>>,
    lastfm_client: Arc<lastfm::Client>,
    slack_client: Arc<SlackClient<SlackClientHyperConnector<SlackHyperHttpsConnector>>>,
}

async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;

    // TODO: figure out if the signing secret is actually useful as an encryption key
    let db = Db::from_encrypted_file(cwd.join("db.json.enc"), env::slack_signing_secret())?;

    let app_state = AppState {
        db: Arc::new(Mutex::new(db)),
        lastfm_client: Arc::new(lastfm::Client::new(
            env::lastfm_key(),
            reqwest::Client::builder()
                .user_agent("slackfm-bot")
                .build()
                .unwrap(),
        )),
        slack_client: Arc::new(SlackClient::new(
            SlackClientHyperConnector::new()?.with_rate_control(SlackApiRateControlConfig::new()),
        )),
    };

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 3000));

    let listener_environment = Arc::new(
        SlackClientEventsListenerEnvironment::new(app_state.slack_client.clone())
            .with_error_handler(error_handler),
    );
    let signing_secret: SlackSigningSecret = env::slack_signing_secret().into();

    let listener: SlackEventsAxumListener<SlackHyperHttpsConnector> =
        SlackEventsAxumListener::new(listener_environment.clone());

    // build our application route with OAuth nested router and Push/Command/Interaction events
    let app = axum::routing::Router::new()
        .route(
            "/command",
            axum::routing::post(command_event).layer(
                listener
                    .events_layer(&signing_secret)
                    .with_event_extractor(SlackEventsExtractors::command_event()),
            ),
        )
        .with_state(app_state.clone())
        .route("/auth", axum::routing::get(oauth_handler))
        .with_state(app_state.clone())
        .route("/installed", axum::routing::get(test_welcome_installed))
        .route("/cancelled", axum::routing::get(test_cancelled_install))
        .route("/error", axum::routing::get(test_error_install))
        .route(
            "/push",
            axum::routing::post(test_push_event).layer(
                listener
                    .events_layer(&signing_secret)
                    .with_event_extractor(SlackEventsExtractors::push_event()),
            ),
        );

    spawn_client_update_loop(app_state.clone()).await;

    axum::serve(TcpListener::bind(&addr).await.unwrap(), app)
        .await
        .unwrap();

    Ok(())
}

async fn spawn_client_update_loop(state: AppState) {
    let db = state.db.lock().await;

    for (slack_user_id, user_data) in db.users() {
        let user_id = SlackUserId::new(slack_user_id.into());
        let lastfm_client = state.lastfm_client.clone();
        tokio::task::spawn(update_user_data(
            state.slack_client.clone(),
            lastfm_client,
            user_id,
            user_data,
        ));
    }
}

async fn update_user_data(
    client: Arc<SlackClient<SlackClientHyperConnector<SlackHyperHttpsConnector>>>,
    lastfm_client: Arc<lastfm::Client>,
    user_id: SlackUserId,
    user_data: Arc<std::sync::Mutex<UserData>>,
) {
    let (lastfm_username, slack_token) = {
        let user_data = user_data.lock().unwrap();
        let lastfm = user_data.lastfm_username().to_owned();
        let slack = user_data.slack_token().map(ToOwned::to_owned);
        (lastfm, slack)
    };

    let Some(slack_token) = slack_token else {
        println!(
            "No slack token for user {}. User didn't authenticate it seems",
            user_id
        );
        return;
    };

    let slack_client = slack::Client::from_client(client, slack_token, env::slack_team_id());

    let stream = lastfm_client.stream_now_playing(&lastfm_username, Duration::from_secs(10));

    pin_mut!(stream);

    while let Some(track) = stream.next().await {
        match track {
            Ok(track) => {
                if let Some(track) = track {
                    println!("updating status for {} to {}", user_id, track.name());
                    println!("TODO: update slack status");
                } else {
                    println!("updating status for {} to not listening/blank", user_id);
                    println!("TODO: update slack status");
                }
            }
            Err(e) => {
                eprintln!("Error: {}", e);
            }
        }
    }
}

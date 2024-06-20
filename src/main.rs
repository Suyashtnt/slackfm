mod db;
pub mod env;
mod oauth;

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::Extension;
use db::{Db, UserData};
use dotenvy::dotenv;
use error_stack::{Result, ResultExt};
use futures::{pin_mut, stream, StreamExt};
use oauth::{create_oauth_client, OauthCode};
use oauth2::reqwest::async_http_client;
use oauth2::{AuthorizationCode, CsrfToken};
use slack_morphism::prelude::*;
use slackfm::{lastfm, slack};
use tokio::sync::oneshot;
use tokio::task::{AbortHandle, JoinHandle};
use tokio::{net::TcpListener, sync::Mutex};
use tracing::{debug, error, info};
use tracing_error::ErrorLayer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::EnvFilter;

#[derive(Debug)]
enum MainError {
    SetupError,
    ServerError,
}

impl fmt::Display for MainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MainError::SetupError => f.write_str("Error setting up the server"),
            MainError::ServerError => f.write_str("Error running the server"),
        }
    }
}

impl Error for MainError {}

#[tokio::main]
async fn main() -> Result<(), MainError> {
    let subscriber = tracing_subscriber::Registry::default()
        .with(tracing_subscriber::fmt::layer())
        .with(ErrorLayer::default())
        .with(EnvFilter::from_default_env());

    tracing::subscriber::set_global_default(subscriber)
        .attach_printable("Error setting up the logger")
        .change_context(MainError::SetupError)?;

    dotenv()
        .attach_printable("Error loading the .env file")
        .change_context(MainError::SetupError)?;

    if env::any_set() {
        env::assert_env_vars();
        run_server()
            .await
            .attach_printable("Error running the server")
            .change_context(MainError::ServerError)
    } else {
        println!("# Environment Variables Help\n{}", env::gen_help());
        Ok(())
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

async fn command_event(
    Extension(_environment): Extension<Arc<SlackHyperListenerEnvironment>>,
    Extension(event): Extension<SlackCommandEvent>,
    State(state): State<AppState>,
) -> axum::Json<SlackCommandEventResponse> {
    match &*event.command.0 {
        "/connect" => connect_handler(event, state).await,
        "/disconnect" => disconnect_handler(event, state).await,
        _ => {
            info!("Received unknown command");
            axum::Json(SlackCommandEventResponse::new(
                SlackMessageContent::new().with_text("Received unknown command".into()),
            ))
        }
    }
}

async fn disconnect_handler(
    event: SlackCommandEvent,
    state: AppState,
) -> axum::Json<SlackCommandEventResponse> {
    info!("Received disconnect command");

    let mut db = state.db.lock().await;
    let user_id = event.user_id;

    match db.remove_user(&user_id.0) {
        Ok(Some(_)) => {
            let abort_handle = state.tasks.lock().await.remove(&user_id.into()).unwrap();
            abort_handle.abort();

            axum::Json(SlackCommandEventResponse::new(
                SlackMessageContent::new().with_text("Disconnected lastfm user".into()),
            ))
        }
        Ok(None) => axum::Json(SlackCommandEventResponse::new(
            SlackMessageContent::new()
                .with_text("You were not found in the database!. Please run /connect".into()),
        )),
        Err(e) => {
            error!("Error removing user {}: {}", user_id, e);
            axum::Json(SlackCommandEventResponse::new(
                SlackMessageContent::new()
                    .with_text(
                        "Error disconnecting your user. A report has been logged on the server"
                            .into(),
                    )
                    .into(),
            ))
        }
    }
}

async fn connect_handler(
    event: SlackCommandEvent,
    state: AppState,
) -> axum::Json<SlackCommandEventResponse> {
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
            SlackMessageContent::new().with_text("No username found. Please give one".into()),
        ));
    };

    // check if the lastfm user exists
    if !state
        .lastfm_client
        .does_user_exist(&lastfm_username)
        .await
        .unwrap_or(false)
    {
        return axum::Json(SlackCommandEventResponse::new(
            SlackMessageContent::new().with_text("The lastfm username isn't valid/doesn't exist. Make sure you're inputting your username from the URL (https://www.last.fm/user/<username>)".into()),
        ));
    }

    let mut db = state.db.lock().await;

    let user = db.user(&event.user_id.0);

    if let Some(user) = user.filter(|user| user.lock().unwrap().slack_token().is_some()) {
        user.lock().unwrap().update_lastfm_username(lastfm_username);
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

        if let Err(e) = db.add_user(event.user_id.0, UserData::new(lastfm_username, csrf_token)) {
            return axum::Json(SlackCommandEventResponse::new(
                SlackMessageContent::new()
                    .with_text(format!("Error adding your user to the database: {}", e)),
            ));
        }

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

async fn oauth_handler(
    Query(code): Query<OauthCode>,
    State(state): State<AppState>,
) -> &'static str {
    let db = state.db.lock().await;

    // Retrieve the csrf token and pkce verifier
    let Some(user_arc) = db.user_with_csrf(&code.state) else {
        return "CSRF couldn't be linked to a user. Theres a middleman attack at play or I didn't save the token properly";
    };

    let client = create_oauth_client();

    let response = client
        .exchange_code(AuthorizationCode::new(code.code))
        .request_async(async_http_client)
        .await
        .unwrap();

    let user_token = response.extra_fields().authed_user.access_token.clone();
    let user_id = response.extra_fields().authed_user.id.clone();

    user_arc.lock().unwrap().promote_token(user_token);

    db.to_encrypted_file().unwrap();

    let user_id: SlackUserId = user_id.into();
    let abort_handle = tokio::task::spawn(update_user_data(
        state.slack_client.clone(),
        state.lastfm_client.clone(),
        user_id.clone(),
        user_arc,
    ))
    .abort_handle();

    state.tasks.lock().await.insert(user_id, abort_handle);

    "Authenticated!"
}

#[derive(Clone)]
struct AppState {
    db: Arc<Mutex<Db>>,
    tasks: Arc<Mutex<HashMap<SlackUserId, AbortHandle>>>,
    lastfm_client: Arc<lastfm::Client>,
    slack_client: Arc<SlackClient<SlackClientHyperConnector<SlackHyperHttpsConnector>>>,
}

#[derive(Debug)]
enum ServerError {
    IoError,
    LastfmError,
    DbError,
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IoError => f.write_str("An IO error occurred"),
            Self::LastfmError => f.write_str("A Last.fm error occurred"),
            Self::DbError => f.write_str("An error occured when setting up the database"),
        }
    }
}

impl Error for ServerError {}

async fn run_server() -> Result<(), ServerError> {
    let cwd = std::env::current_dir()
        .attach_printable("Couldn't get current working directory.")
        .change_context(ServerError::IoError)?;

    let db = Db::from_encrypted_file(cwd.join("db.json.enc"), env::slack_signing_secret())
        .attach_printable("Couldn't load the database.")
        .change_context(ServerError::DbError)?;

    let app_state = AppState {
        db: Arc::new(Mutex::new(db)),
        tasks: Arc::new(Mutex::new(HashMap::new())),
        lastfm_client: Arc::new(lastfm::Client::new(
            env::lastfm_key(),
            reqwest::Client::builder()
                .user_agent("slackfm-bot")
                .build()
                .attach_printable("Couldn't create the Lastfm client HTTP connector.")
                .change_context(ServerError::IoError)?,
        )),
        slack_client: Arc::new(SlackClient::new(
            SlackClientHyperConnector::new()
                .attach_printable("Couldn't create the Slack client HTTP connector.")
                .change_context(ServerError::IoError)?
                .with_rate_control(SlackApiRateControlConfig::new()),
        )),
    };

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 5127));

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
        .with_state(app_state.clone());

    spawn_initial_updaters(app_state.clone())
        .await
        .attach_printable("Couldn't spawn the initial updaters.")
        .change_context(ServerError::LastfmError)?;

    axum::serve(
        TcpListener::bind(&addr)
            .await
            .attach_printable("Couldn't bind listener to the address.")
            .change_context(ServerError::IoError)?,
        app,
    )
    .await
    .unwrap();

    Ok(())
}

async fn spawn_initial_updaters(state: AppState) -> Result<(), ServerError> {
    let mut db = state.db.lock().await;

    db.map_db(|hashmap| {
        stream::iter(hashmap.into_iter())
            .filter(|(_, user_data)| {
                let lastfm_client = state.lastfm_client.clone();
                let lastfm_username = user_data.lock().unwrap().lastfm_username().to_owned();
                async move {
                    lastfm_client
                        .does_user_exist(&lastfm_username)
                        .await
                        .unwrap_or(false)
                }
            })
            .collect()
    })
    .await
    .attach_printable("Couldn't remove bad users from the database.")
    .change_context(ServerError::DbError)?;

    for (slack_user_id, user_data) in db.users() {
        let user_id = SlackUserId::new(slack_user_id.into());
        let lastfm_client = state.lastfm_client.clone();
        let abort_handle = tokio::task::spawn(update_user_data(
            state.slack_client.clone(),
            lastfm_client,
            user_id.clone(),
            user_data,
        ))
        .abort_handle();

        state.tasks.lock().await.insert(user_id, abort_handle);
    }

    Ok(())
}

#[tracing::instrument(skip(client, lastfm_client, user_data))]
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
        info!(
            "No slack token for user {}. User didn't authenticate it seems",
            user_id
        );
        return;
    };

    let slack_client = slack::Client::from_client(client, slack_token, env::slack_team_id());

    let stream = lastfm_client.stream_now_playing(&lastfm_username, Duration::from_secs(10));

    pin_mut!(stream);

    info!("Polling user data for user {}", user_id);

    while let Some(track) = stream.next().await {
        debug!("Got track: {:?}", track);
        match track {
            Ok(track) => {
                if let Some(track) = track {
                    println!("updating status for {} to {}", &user_id, track.name());
                    if let Err(e) = slack_client
                        .update_user_status(
                            user_id.clone(),
                            Some(format!("{} - {}", track.name(), track.artist())),
                            Some(":music:"),
                            // We can't get the song length from lastfm, so we'll pretend it lasts forever :clueless:
                            None,
                        )
                        .await
                    {
                        error!("Error setting status for {}: {:#?}", &user_id, e);
                    }
                } else {
                    println!("updating status for {} to not listening/blank", user_id);
                    if let Err(e) = slack_client
                        .update_user_status(user_id.clone(), Some(""), Some(""), None)
                        .await
                    {
                        error!("Error setting status for {}: {:#?}", &user_id, e);
                    }
                }
            }
            Err(e) => {
                error!("Error: {:#?}", e);
            }
        }
    }
}

mod db;

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::Extension;
use db::Db;
use dotenvy::dotenv;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::Response;
use slack_morphism::prelude::*;
use slackfm::lastfm;
use tokio::net::TcpListener;

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

        slack_redirect_host, "SLACK_REDIRECT_HOST", String,
        "Please set your slack redirect host URI in the environment variable SLACK_REDIRECT_HOST";
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

async fn test_oauth_install_function(
    resp: SlackOAuthV2AccessTokenResponse,
    _client: Arc<SlackHyperClient>,
    _states: SlackClientEventsUserState,
) {
    println!("{:#?}", resp);
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

            let Some(username) = event
                .text
                .and_then(|text| text.split_once(" ").map(|split| split.0.to_owned()))
            else {
                return axum::Json(SlackCommandEventResponse::new(
                    SlackMessageContent::new()
                        .with_text("No username found. Please give one".into()),
                ));
            };

            let db = state.db.lock().unwrap();

            let user = db.user(&event.user_id.0);

            if let Some(user) = user {
                let mut user = user.lock().unwrap();
                // update their last.fm username
                user.update_lastfm_username(username);

                axum::Json(SlackCommandEventResponse::new(
                    SlackMessageContent::new().with_text("Updated Last.fm username".into()),
                ))
            } else {
                axum::Json(SlackCommandEventResponse::new(
                    SlackMessageContent::new().with_text("Working on it".into()),
                ))
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

async fn test_interaction_event(
    Extension(_environment): Extension<Arc<SlackHyperListenerEnvironment>>,
    Extension(event): Extension<SlackInteractionEvent>,
) {
    println!("Received interaction event: {:?}", event);
}

#[derive(Clone)]
struct AppState {
    db: Arc<Mutex<Db>>,
    lastfm_client: Arc<lastfm::Client>,
}

async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;

    // TODO: figure out if the signing secret is actually useful as an encryption key
    let db = Db::from_encrypted_file(&cwd.join("db.json.enc"), &env::slack_signing_secret())
        .unwrap_or_else(Db::new);

    let app_state = AppState {
        db: Arc::new(Mutex::new(db)),
    };

    let client = Arc::new(SlackClient::new(
        SlackClientHyperConnector::new()?.with_rate_control(SlackApiRateControlConfig::new()),
    ));

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 3000));
    let oauth_config = SlackOAuthListenerConfig::new(
        env::slack_client_id().into(),
        env::slack_client_secret().into(),
        "commands".to_string(),
        env::slack_redirect_host(),
    );

    let listener_environment = Arc::new(
        SlackClientEventsListenerEnvironment::new(client.clone()).with_error_handler(error_handler),
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
        .nest(
            "/auth",
            listener.oauth_router("/auth", &oauth_config, test_oauth_install_function),
        )
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
        )
        .route(
            "/interaction",
            axum::routing::post(test_interaction_event).layer(
                listener
                    .events_layer(&signing_secret)
                    .with_event_extractor(SlackEventsExtractors::interaction_event()),
            ),
        );

    spawn_client_update_loop(client.clone(), app_state.clone());

    axum::serve(TcpListener::bind(&addr).await.unwrap(), app)
        .await
        .unwrap();

    Ok(())
}

async fn spawn_client_update_loop(
    client: Arc<SlackClient<SlackClientHyperConnector<SlackHyperHttpsConnector>>>,
    state: AppState,
) {
    let db = state.db.lock().unwrap();

    for (slack_user_id, user_data) in db.users() {
        let user_id = SlackUserId::new(slack_user_id.into());
        let lastfm_client = state.lastfm_client.clone();
        tokio::task::spawn(async move { todo!() });
    }
}

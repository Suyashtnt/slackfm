use std::sync::Arc;

use dotenvy::dotenv;
use slack_morphism::prelude::*;
use slackfm::slack;

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

        run_server().await;
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

async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let team_id = env::slack_team_id();

    let client = SlackClient::new(
        SlackClientHyperConnector::new()?.with_rate_control(SlackApiRateControlConfig::new()),
    );

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 3000));
    let oauth_config = SlackOAuthListenerConfig::new(
        env::slack_client_id().into(),
        env::slack_client_secret().into(),
        env::slack_signing_secret(),
        env::slack_redirect_host(),
    );

    let listener_environment = Arc::new(
        SlackClientEventsListenerEnvironment::new(client.clone()).with_error_handler(error_handler),
    );

    Ok(())
}

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

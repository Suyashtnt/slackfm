use std::time::Duration;

use dotenvy::dotenv;
use futures::{pin_mut, StreamExt};
use slackfm::lastfm;

mod env {
    use menv::require_envs;
    require_envs! {
        (assert_env_vars, any_set, gen_help);

        lastfm_key, "LASTFM_API_KEY", String,
        "Please set your last.fm API key in the environment variable LASTFM_API_KEY";
    }
}

#[tokio::main]
async fn main() {
    dotenv().expect(".env file not found");

    if env::any_set() {
        env::assert_env_vars();

        let username = std::env::args()
            .nth(1)
            .expect("Please provide a last.fm username as the first argument");

        let api_key = env::lastfm_key();
        let api_client = reqwest::Client::builder()
            .user_agent("slackfm")
            .build()
            .unwrap();
        let client = lastfm::Client::new(api_key, api_client);

        let recent_tracks = client
            .get_user_recent_tracks(&username)
            .await
            .expect("Failed to fetch users recent tracks");

        let recent_track = recent_tracks.first().unwrap();

        println!(
            "{}'s most recent track: {} - {}",
            username,
            recent_track.name(),
            recent_track.artist()
        );

        run_polling(client, username).await;
    } else {
        println!("# Environment Variables Help\n{}", env::gen_help());
        return;
    }
}

async fn run_polling(client: lastfm::Client, username: String) {
    loop {
        let stream = client.stream_now_playing(&username, Duration::from_secs(3));

        pin_mut!(stream);

        while let Some(track) = stream.next().await {
            match track {
                Ok(track) => {
                    if let Some(track) = track {
                        println!(
                            "{} is now listening to {} - {} from the album {}",
                            username,
                            track.name(),
                            track.artist(),
                            track.album()
                        );
                    } else {
                        println!("{} is not listening to anything", username);
                    }
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                }
            }
        }
    }
}

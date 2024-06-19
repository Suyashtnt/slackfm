use std::time::Duration;

use async_stream::try_stream;
use futures::Stream;
use nestify::nest;
use url::Url;

pub const API_BASE: &str = "http://ws.audioscrobbler.com/2.0/";

pub struct Client {
    key: String,
    client: reqwest::Client,
    base_url: Url,
}

impl Client {
    pub fn new(api_key: String, client: reqwest::Client) -> Self {
        Self {
            key: api_key,
            client,
            base_url: Url::parse(API_BASE).unwrap(),
        }
    }

    pub async fn get_user_recent_tracks(
        &self,
        user: &str,
    ) -> Result<Vec<RecentTrack>, reqwest::Error> {
        let mut cloned_url = self.base_url.clone();
        let url = cloned_url
            .query_pairs_mut()
            .append_pair("method", "user.getrecenttracks")
            .append_pair("user", user)
            .append_pair("api_key", &self.key)
            .append_pair("format", "json")
            .finish();

        let response = self
            .client
            .get(url.as_ref())
            .send()
            .await?
            .json::<RecentTracksResponse>()
            .await?;

        Ok(response
            .recenttracks
            .track
            .into_iter()
            .map(Into::into)
            .collect())
    }

    // A stream of the currently playing track
    //
    // # Returns
    // returns a new track if a user is playing something new, else returns None if the user has stopped playing anything
    pub async fn stream_now_playing<'a>(
        &'a self,
        user: &'a str,
        polling_interval: Duration,
    ) -> impl Stream<Item = Result<Option<RecentTrack>, reqwest::Error>> + 'a {
        let mut last_played_track: Option<RecentTrack> = None;
        try_stream! {
            loop {
                let tracks = self.get_user_recent_tracks(user).await?;
                let now_playing = tracks
                    .into_iter()
                    .find(|track| track.is_now_playing);

                // return early if the user isn't playing anything now
                let Some(now_playing) = now_playing else {
                    if last_played_track != None {
                        last_played_track = None;
                        yield None;
                    }
                    continue;
                };

                let Some(ref last_playing) = last_played_track else {
                    // if we weren't playing anything before, then we're playing something new
                    last_played_track = Some(now_playing.clone());
                    yield Some(now_playing);
                    continue;
                };

                // else, check if the user is playing a new track through checking the mbid and name

                // make sure the mbid is not empty before checking if they're different
                if now_playing.mbid != "" {
                    if now_playing.mbid != last_playing.mbid {
                        last_played_track = Some(now_playing.clone());
                        yield Some(now_playing);
                    }
                } else {
                   if now_playing.name != last_playing.name {
                        last_played_track = Some(now_playing.clone());
                        yield Some(now_playing);
                    }
                }

                // wait for the next poll
                tokio::time::sleep(polling_interval).await;
                continue;
            }
        }
    }
}

nest! {
    #[derive(serde::Deserialize)]*
    /// Last.fm API response for the `user.getrecenttracks` method.
    /// Limited to only the fields we care about.
    struct RecentTracksResponse {
        recenttracks: struct RecentTracksInner {
            track: Vec<struct Track {
                name: String,
                mbid: String,
                artist: struct Artist {
                    #[serde(rename = "#text")]
                    text: String,
                },
                image: Vec<struct Image {
                    #>[derive(PartialEq, Eq)]
                    size: enum ImageSize {
                        #[serde(rename = "small")]
                        Small,
                        #[serde(rename = "medium")]
                        Medium,
                        #[serde(rename = "large")]
                        Large,
                        #[serde(rename = "extralarge")]
                        ExtraLarge,
                    },
                    #[serde(rename = "#text")]
                    url: Url,
                }>,
                album: struct Album {
                    #[serde(rename = "#text")]
                    text: String,
                },
                #[serde(rename = "@attr")]
                attr: Option<struct TrackAttributes {
                    #[serde(rename = "nowplaying")]
                    now_playing: Option<bool>,
                }>,
            }>,
        },
    }
}

/// Parsed response from the `user.getrecenttracks` method.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct RecentTrack {
    mbid: String,
    name: String,
    artist: String,
    album: String,
    image_url: Url,
    is_now_playing: bool,
}

impl From<Track> for RecentTrack {
    fn from(track: Track) -> Self {
        let image_url = track
            .image
            .into_iter()
            .find(|image| image.size == ImageSize::Medium)
            .map(|image| image.url)
            .unwrap_or_else(|| Url::parse("https://via.placeholder.com/64").unwrap());

        Self {
            name: track.name,
            artist: track.artist.text,
            album: track.album.text,
            image_url,
            is_now_playing: track
                .attr
                .map_or(false, |attr| attr.now_playing.unwrap_or(false)),
        }
    }
}

#[cfg(test)]
mod tests {
    use dotenvy_macro::dotenv;

    use super::*;

    const API_KEY: &str = dotenv!("LASTFM_API_KEY");

    #[tokio::test]
    async fn can_create_client() {
        // make sure it doesn't panic
        Client::new(API_KEY.to_owned(), reqwest::Client::new());
    }

    #[tokio::test]
    async fn can_get_user_recent_tracks() {
        let client = Client::new(API_KEY.to_owned(), reqwest::Client::new());
        let tracks = client.get_user_recent_tracks("rj").await.unwrap();
        assert!(!tracks.is_empty());
    }

    #[tokio::test]
    async fn errors_on_nonexistent_user() {
        let client = Client::new(API_KEY.to_owned(), reqwest::Client::new());

        // if somebody spites me and registers this username, you're quite mean :(
        let tracks = client
            .get_user_recent_tracks("asdklqweyhtuiowhfasdlfjasdiofho")
            .await;

        assert!(tracks.is_err());
    }
}

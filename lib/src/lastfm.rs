use std::{error::Error, fmt, time::Duration};

use async_stream::try_stream;
use error_stack::{Result, ResultExt};
use futures::Stream;
use nestify::nest;
use tracing::debug;
use url::Url;

pub const API_BASE: &str = "https://ws.audioscrobbler.com/2.0/";

pub struct Client {
    key: String,
    client: reqwest::Client,
    base_url: Url,
}

#[derive(Debug)]
pub enum LastFMError {
    RequestError,
    ParseError,
}

impl fmt::Display for LastFMError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LastFMError::RequestError => f.write_str("An error occurred while making the request"),
            LastFMError::ParseError => f.write_str("An error occurred while parsing the response"),
        }
    }
}
impl Error for LastFMError {}

impl Client {
    pub fn new(api_key: String, client: reqwest::Client) -> Self {
        Self {
            key: api_key,
            client,
            base_url: Url::parse(API_BASE).unwrap(),
        }
    }

    #[tracing::instrument(skip(self))]
    pub async fn does_user_exist(&self, user: &str) -> Result<bool, LastFMError> {
        let mut cloned_url = self.base_url.clone();

        let url = cloned_url
            .query_pairs_mut()
            .append_pair("method", "user.getinfo")
            .append_pair("user", user)
            .append_pair("api_key", &self.key)
            .append_pair("format", "json")
            .finish();

        debug!("Requesting user info from LastFM: {}", url.as_ref());

        let response = self
            .client
            .get(url.as_ref())
            .send()
            .await
            .attach_printable("Couldn't send request")
            .change_context(LastFMError::RequestError)?
            .json::<UserInfoResponse>()
            .await
            .attach_printable("Couldn't deserialise response")
            .change_context(LastFMError::ParseError)?;

        debug!("Response form lastFM: {:?}", response);

        Ok(response.user.is_some())
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_user_recent_tracks(
        &self,
        user: &str,
    ) -> Result<Vec<RecentTrack>, LastFMError> {
        let mut cloned_url = self.base_url.clone();
        let url = cloned_url
            .query_pairs_mut()
            .append_pair("method", "user.getrecenttracks")
            .append_pair("user", user)
            .append_pair("api_key", &self.key)
            .append_pair("format", "json")
            .finish();

        debug!("Requesting recent tracks from LastFM: {}", url.as_ref());

        let response = self
            .client
            .get(url.as_ref())
            .send()
            .await
            .attach_printable("Couldn't send request")
            .change_context(LastFMError::RequestError)?
            .json::<RecentTracksResponse>()
            .await
            .attach_printable("Couldn't deserialise response")
            .change_context(LastFMError::ParseError)?;

        debug!("Response from LastFM: {:?}", response);

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
    #[tracing::instrument(skip(self))]
    pub fn stream_now_playing<'a>(
        &'a self,
        user: &'a str,
        polling_interval: Duration,
    ) -> impl Stream<Item = Result<Option<RecentTrack>, LastFMError>> + 'a {
        let mut last_playing: Option<RecentTrack> = None;
        try_stream! {
            loop {
                // wait before the next poll
                tokio::time::sleep(polling_interval).await;

                debug!("Polling LastFM for now playing track for {user}");
                let tracks = self.get_user_recent_tracks(user).await?;

                let now_playing = tracks
                    .into_iter()
                    .find(|track| track.is_now_playing);

                debug!("User {user} is now playing: {:?}", now_playing);

                match (now_playing, last_playing.clone()) {
                    // the user is not playing anything nor has played anything before
                    (None, None) => {
                        debug!("User {user} is not playing anything");
                    },
                    // The user has started to play their first song
                    (Some(playing), None) => {
                        debug!("User {user} is now playing their first song: {playing}");
                        last_playing = Some(playing.clone());
                        yield Some(playing);
                    },
                    // The user has stopped playing anything
                    (None, Some(_)) => {
                        debug!("User {user} has stopped playing anything");
                        last_playing = None;
                        yield None;
                    },
                    // The user is playing a new track
                    (Some(playing), Some(last)) => {
                        debug!("User {user} is now playing a new track: {playing}. Checking if it's different from the last track: {last}");
                        // make sure the mbid is not empty before checking if they're different
                        if playing.mbid != "" {
                            if playing.mbid != last.mbid {
                                last_playing = Some(playing.clone());
                                yield Some(playing);
                            }
                        } else if playing.name != last.name {
                            last_playing = Some(playing.clone());
                            yield Some(playing);
                        }
                    },
                }

                continue;
            }
        }
    }
}

nest! {
    #[derive(serde::Deserialize, Debug)]*
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
                    now_playing: Option<String>,
                }>,
            }>,
        },
    }
}

nest! {
    #[derive(serde::Deserialize, Debug)]*
    /// Last.fm API response for the `user.getinfo` method.
    /// Limited to only the fields we care about.
    struct UserInfoResponse {
        user: Option<struct User {}>,
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

impl fmt::Display for RecentTrack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} - {}", self.name, self.artist)
    }
}

impl RecentTrack {
    pub fn mbid(&self) -> &str {
        &self.mbid
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn artist(&self) -> &str {
        &self.artist
    }

    pub fn album(&self) -> &str {
        &self.album
    }

    pub fn image_url(&self) -> &Url {
        &self.image_url
    }

    pub fn is_now_playing(&self) -> bool {
        self.is_now_playing
    }
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
            mbid: track.mbid,
            artist: track.artist.text,
            album: track.album.text,
            image_url,
            is_now_playing: track.attr.map_or(false, |attr| {
                attr.now_playing.unwrap_or_else(|| "false".to_string()) == "true"
            }),
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

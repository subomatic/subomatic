// SPDX-License-Identifier: Apache-2.0
//! A minimal client for the OpenSubtitles REST API
//! (`https://api.opensubtitles.com/api/v1`).
//!
//! Native-only (uses `ureq`); the browser front-end uses the platform `fetch`
//! instead. Supply your own API key and a descriptive `User-Agent` — both are
//! required by OpenSubtitles. Search needs only the API key; downloads also need
//! a login token (and count against your daily quota).
//!
//! Live calls require a key/account, so the unit tests cover only the
//! request-shaping and response-parsing logic.

use serde::Deserialize;
use std::fmt;

const BASE: &str = "https://api.opensubtitles.com/api/v1";

/// An error from the OpenSubtitles client.
#[derive(Debug)]
pub enum Error {
    /// Transport / IO failure.
    Http(String),
    /// The API returned a non-success status.
    Api { status: u16, body: String },
    /// A response could not be parsed.
    Parse(String),
    /// A download was attempted before [`Client::login`].
    NotLoggedIn,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Http(e) => write!(f, "HTTP error: {e}"),
            Error::Api { status, body } => {
                let snippet: String = body.chars().take(500).collect();
                write!(f, "OpenSubtitles API error {status}: {snippet}")
            }
            Error::Parse(e) => write!(f, "failed to parse response: {e}"),
            Error::NotLoggedIn => write!(f, "login is required before downloading"),
        }
    }
}

impl std::error::Error for Error {}

/// Search parameters; combine any of these.
#[derive(Debug, Default, Clone)]
pub struct SearchQuery {
    /// Free-text query (movie / episode title).
    pub query: Option<String>,
    /// Comma-separated language codes, e.g. `"en"` or `"en,fr"`.
    pub languages: Option<String>,
    /// OpenSubtitles moviehash of the media file.
    pub moviehash: Option<String>,
    /// IMDb id (without the `tt` prefix).
    pub imdb_id: Option<String>,
}

impl SearchQuery {
    /// The query parameters to send, in a stable order (only set fields).
    pub fn params(&self) -> Vec<(&'static str, String)> {
        let mut params = Vec::new();
        if let Some(v) = &self.query {
            params.push(("query", v.clone()));
        }
        if let Some(v) = &self.languages {
            params.push(("languages", v.clone()));
        }
        if let Some(v) = &self.moviehash {
            params.push(("moviehash", v.clone()));
        }
        if let Some(v) = &self.imdb_id {
            params.push(("imdb_id", v.clone()));
        }
        params
    }
}

/// One search hit, flattened to the fields a caller needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subtitle {
    pub file_id: i64,
    pub file_name: String,
    pub language: String,
    pub release: String,
    pub download_count: i64,
}

/// A client for the OpenSubtitles REST API.
pub struct Client {
    api_key: String,
    user_agent: String,
    token: Option<String>,
    agent: ureq::Agent,
}

impl Client {
    /// Create a client. `user_agent` must identify your app (required by the API).
    pub fn new(api_key: impl Into<String>, user_agent: impl Into<String>) -> Self {
        Client {
            api_key: api_key.into(),
            user_agent: user_agent.into(),
            token: None,
            agent: ureq::agent(),
        }
    }

    /// Log in to obtain a token (required for downloads; raises the quota).
    pub fn login(&mut self, username: &str, password: &str) -> Result<(), Error> {
        let body = serde_json::json!({ "username": username, "password": password });
        let text = handle(self.post(&format!("{BASE}/login")).send_json(body))?;
        let parsed: LoginResponse = parse(&text)?;
        self.token = Some(parsed.token);
        Ok(())
    }

    /// Search for subtitles. Hits without a downloadable file are skipped.
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<Subtitle>, Error> {
        let mut request = self.get(&format!("{BASE}/subtitles"));
        for (key, value) in query.params() {
            request = request.query(key, &value);
        }
        parse_search(&handle(request.call())?)
    }

    /// Download a subtitle's text by its `file_id`. Requires a prior [`Client::login`].
    pub fn download(&self, file_id: i64) -> Result<String, Error> {
        if self.token.is_none() {
            return Err(Error::NotLoggedIn);
        }
        let body = serde_json::json!({ "file_id": file_id });
        let link = parse_download(&handle(
            self.post(&format!("{BASE}/download")).send_json(body),
        )?)?;
        handle(
            self.agent
                .get(&link)
                .set("User-Agent", &self.user_agent)
                .call(),
        )
    }

    fn get(&self, url: &str) -> ureq::Request {
        self.with_headers(self.agent.get(url))
    }

    fn post(&self, url: &str) -> ureq::Request {
        // `send_json` sets Content-Type: application/json for us.
        self.with_headers(self.agent.post(url))
    }

    fn with_headers(&self, request: ureq::Request) -> ureq::Request {
        let request = request
            .set("Api-Key", &self.api_key)
            .set("User-Agent", &self.user_agent)
            .set("Accept", "application/json");
        match &self.token {
            Some(token) => request.set("Authorization", &format!("Bearer {token}")),
            None => request,
        }
    }
}

/// Turn a `ureq` result into a body string or a classified [`Error`].
fn handle(result: Result<ureq::Response, ureq::Error>) -> Result<String, Error> {
    match result {
        Ok(response) => response
            .into_string()
            .map_err(|e| Error::Http(e.to_string())),
        Err(ureq::Error::Status(status, response)) => Err(Error::Api {
            status,
            body: response
                .into_string()
                .unwrap_or_else(|e| format!("<error body unavailable: {e}>")),
        }),
        Err(other) => Err(Error::Http(other.to_string())),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T, Error> {
    serde_json::from_str(text).map_err(|e| Error::Parse(e.to_string()))
}

fn parse_search(text: &str) -> Result<Vec<Subtitle>, Error> {
    let response: SearchResponse = parse(text)?;
    Ok(response
        .data
        .into_iter()
        .filter_map(into_subtitle)
        .collect())
}

fn parse_download(text: &str) -> Result<String, Error> {
    let response: DownloadResponse = parse(text)?;
    Ok(response.link)
}

fn into_subtitle(datum: SearchDatum) -> Option<Subtitle> {
    let file = datum.attributes.files.into_iter().next()?;
    Some(Subtitle {
        file_id: file.file_id,
        file_name: file.file_name,
        language: datum.attributes.language.unwrap_or_default(),
        release: datum.attributes.release,
        download_count: datum.attributes.download_count,
    })
}

#[derive(Deserialize)]
struct LoginResponse {
    token: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    data: Vec<SearchDatum>,
}

#[derive(Deserialize)]
struct SearchDatum {
    attributes: Attributes,
}

#[derive(Deserialize)]
struct Attributes {
    language: Option<String>,
    #[serde(default)]
    download_count: i64,
    #[serde(default)]
    release: String,
    #[serde(default)]
    files: Vec<FileRef>,
}

#[derive(Deserialize)]
struct FileRef {
    file_id: i64,
    #[serde(default)]
    file_name: String,
}

#[derive(Deserialize)]
struct DownloadResponse {
    link: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_params_include_only_set_fields() {
        let q = SearchQuery {
            query: Some("the matrix".to_string()),
            languages: Some("en".to_string()),
            ..Default::default()
        };
        assert_eq!(
            q.params(),
            vec![
                ("query", "the matrix".to_string()),
                ("languages", "en".to_string()),
            ]
        );
        assert!(SearchQuery::default().params().is_empty());
    }

    #[test]
    fn parses_a_search_response() {
        let json = r#"{
            "total_count": 1,
            "data": [{
                "id": "123",
                "type": "subtitle",
                "attributes": {
                    "language": "en",
                    "download_count": 42,
                    "release": "The.Matrix.1999.1080p",
                    "files": [{ "file_id": 9876, "file_name": "matrix.srt" }]
                }
            }]
        }"#;
        let hits = parse_search(json).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file_id, 9876);
        assert_eq!(hits[0].language, "en");
        assert_eq!(hits[0].download_count, 42);
        assert_eq!(hits[0].file_name, "matrix.srt");
    }

    #[test]
    fn parses_a_download_link() {
        let json = r#"{ "link": "https://dl.opensubtitles.com/x/matrix.srt", "remaining": 99 }"#;
        assert_eq!(
            parse_download(json).unwrap(),
            "https://dl.opensubtitles.com/x/matrix.srt"
        );
    }

    #[test]
    fn skips_hits_with_no_files() {
        let json = r#"{ "data": [{ "attributes": { "language": "en", "files": [] } }] }"#;
        assert!(parse_search(json).unwrap().is_empty());
    }

    #[test]
    fn download_without_login_errors() {
        let client = Client::new("key", "subomatic/0.1 (test)");
        assert!(matches!(client.download(1), Err(Error::NotLoggedIn)));
    }
}

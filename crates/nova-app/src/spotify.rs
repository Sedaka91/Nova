//! Spotify login with Authorization Code and PKCE, and access tokens from KWallet.
//!
//! Verifier, state, client id and tokens never reach the log. The login prints
//! the consent URL to the user's own terminal, nowhere else.

use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::commands::{ContentKind, Reply};
use crate::{secrets, tools};

const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const API_URL: &str = "https://api.spotify.com/v1";
const REDIRECT_URI: &str = "http://127.0.0.1:8898/callback";
const LISTEN_ADDRESS: &str = "127.0.0.1:8898";
const CALLBACK_PATH: &str = "/callback";
const SCOPES: &str = "user-modify-playback-state user-read-playback-state playlist-read-private";

const CLIENT_ID_ACCOUNT: &str = "spotify_client_id";
const REFRESH_TOKEN_ACCOUNT: &str = "spotify_refresh_token";

/// How long the login waits for the browser to come back.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(120);
/// Pause between two looks at the non-blocking listener.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Browsers open idle connections ahead of time; none may hold up the login for long.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound for one request on the listener.
const MAX_REQUEST_BYTES: u64 = 16 * 1024;
/// Random bytes behind the verifier and the state; encodes to 43 characters.
const RANDOM_BYTES: usize = 32;

/// Everything but the RFC 3986 unreserved characters.
const QUERY_VALUE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

const PAGE_DONE: &str = "<!doctype html><meta charset=\"utf-8\"><title>Nova</title><p>Nova hat die Antwort von Spotify erhalten. Du kannst dieses Fenster schließen.</p>";
const PAGE_REJECTED: &str = "<!doctype html><meta charset=\"utf-8\"><title>Nova</title><p>Die Anmeldung bei Spotify ist fehlgeschlagen.</p>";

/// Why a Spotify command failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SpotifyError {
    #[error("not logged in to Spotify")]
    NotLoggedIn,
    #[error("Spotify is unreachable")]
    Unreachable,
    #[error("no Spotify device available")]
    NoDevice,
    #[error("nothing found on Spotify")]
    NotFound,
}

/// Tokens from the Spotify token endpoint.
#[derive(Deserialize)]
pub struct Tokens {
    #[serde(rename = "access_token")]
    pub access: String,
    #[serde(rename = "refresh_token")]
    pub refresh: Option<String>,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens").finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct TokenFailure {
    error: String,
}

/// One request that reached the login listener.
#[derive(Debug, PartialEq, Eq)]
enum Callback {
    /// Not meant for the login, such as the browser asking for a favicon.
    Ignored,
    /// The callback itself, but denied or with a wrong state.
    Rejected,
    Code(String),
}

/// PKCE S256 challenge for `verifier`.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// URL of the Spotify consent page.
pub fn authorize_url(client_id: &str, challenge: &str, state: &str) -> String {
    let encode = |value| utf8_percent_encode(value, QUERY_VALUE);
    format!(
        "{AUTHORIZE_URL}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge_method=S256&code_challenge={}&state={}",
        encode(client_id),
        encode(REDIRECT_URI),
        encode(SCOPES),
        encode(challenge),
        encode(state),
    )
}

/// Query string of `GET /callback`, `None` for any other request.
fn callback_query(request_line: &str) -> Option<&str> {
    let mut parts = request_line.split_ascii_whitespace();
    let (method, target) = (parts.next()?, parts.next()?);
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    (method == "GET" && path == CALLBACK_PATH).then_some(query)
}

/// Authorization code from the callback's HTTP request line, if `state` matches.
pub fn parse_callback(request_line: &str, state: &str) -> Option<String> {
    let query = callback_query(request_line)?;
    let mut code = None;
    let mut state_matches = false;
    for (key, value) in query.split('&').filter_map(|pair| pair.split_once('=')) {
        match key {
            "code" => code = Some(value),
            "state" => state_matches = value == state,
            "error" => return None,
            _ => {}
        }
    }
    if !state_matches {
        return None;
    }
    let code = percent_decode_str(code?).decode_utf8().ok()?;
    (!code.is_empty()).then(|| code.into_owned())
}

fn classify(request_line: &str, state: &str) -> Callback {
    if callback_query(request_line).is_none() {
        return Callback::Ignored;
    }
    parse_callback(request_line, state).map_or(Callback::Rejected, Callback::Code)
}

/// Interpret a response of the token endpoint.
///
/// # Errors
///
/// [`SpotifyError::NotLoggedIn`] when Spotify revoked the grant,
/// [`SpotifyError::Unreachable`] for every other failure.
pub fn parse_token(status: u16, body: &str) -> Result<Tokens, SpotifyError> {
    if status == 200 {
        // A serde error may quote the body, so it stays out of the log.
        return serde_json::from_str(body).map_err(|_| SpotifyError::Unreachable);
    }
    let revoked = status == 400
        && serde_json::from_str::<TokenFailure>(body)
            .is_ok_and(|failure| failure.error == "invalid_grant");
    Err(if revoked {
        SpotifyError::NotLoggedIn
    } else {
        SpotifyError::Unreachable
    })
}

/// POST `form` to the token endpoint.
fn request_tokens(form: &[(&str, &str)]) -> Result<Tokens, SpotifyError> {
    let mut response = tools::http_agent()
        .post(TOKEN_URL)
        .send_form(form.iter().copied())
        .map_err(|err| {
            tracing::warn!(%err, "Spotify token request failed");
            SpotifyError::Unreachable
        })?;
    let status = response.status().as_u16();
    if status != 200 {
        tracing::warn!(status, "Spotify token endpoint returned an error");
    }
    let body = response.body_mut().read_to_string().map_err(|err| {
        tracing::warn!(%err, "Spotify token response unreadable");
        SpotifyError::Unreachable
    })?;
    parse_token(status, &body)
}

/// Fetch a fresh access token with the refresh token from KWallet.
///
/// A refresh token delivered along with it replaces the stored one.
///
/// # Errors
///
/// [`SpotifyError::NotLoggedIn`] when an entry is missing in KWallet or the grant was
/// revoked, [`SpotifyError::Unreachable`] when KWallet cannot be read, for example while
/// locked, or the token request fails.
pub fn access_token() -> Result<String, SpotifyError> {
    let client_id = stored(CLIENT_ID_ACCOUNT)?;
    let refresh_token = stored(REFRESH_TOKEN_ACCOUNT)?;
    let tokens = request_tokens(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", &refresh_token),
        ("client_id", &client_id),
    ])?;
    if let Some(new_token) = tokens.refresh
        && let Err(err) = secrets::store(REFRESH_TOKEN_ACCOUNT, &new_token)
    {
        tracing::warn!(err = %format_args!("{err:#}"), "storing the new Spotify refresh token failed");
    }
    Ok(tokens.access)
}

/// A failed read says nothing about the login, only that KWallet did not answer.
fn stored_value(read: anyhow::Result<Option<String>>) -> Result<String, SpotifyError> {
    match read {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err(SpotifyError::NotLoggedIn),
        Err(_) => Err(SpotifyError::Unreachable),
    }
}

fn stored(account: &str) -> Result<String, SpotifyError> {
    let read = secrets::read(account).inspect_err(|err| {
        tracing::warn!(err = %format_args!("{err:#}"), account, "reading from KWallet failed");
    });
    stored_value(read)
}

/// One-time login in the terminal; stores client id and refresh token in KWallet.
///
/// # Errors
///
/// Fails when no client id is entered, the port is taken, the browser does not come
/// back within 120 s, Spotify denies the login or KWallet does not accept the values.
pub fn login() -> anyhow::Result<()> {
    let client_id = prompt_client_id()?;
    let verifier = random_token()?;
    let state = random_token()?;
    let listener =
        TcpListener::bind(LISTEN_ADDRESS).with_context(|| format!("binding {LISTEN_ADDRESS}"))?;
    listener
        .set_nonblocking(true)
        .context("configuring the login listener")?;

    let url = authorize_url(&client_id, &challenge(&verifier), &state);
    println!(
        "Melde dich im Browser bei Spotify an. Falls er sich nicht öffnet, rufe diese Adresse auf:\n{url}"
    );
    open_browser(url);

    let code = wait_for_code(&listener, &state)?;
    let tokens = request_tokens(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", &client_id),
        ("code_verifier", &verifier),
    ])
    .context("exchanging the authorization code")?;
    let refresh_token = tokens.refresh.context("Spotify sent no refresh token")?;
    secrets::store(CLIENT_ID_ACCOUNT, &client_id).context("storing the client id")?;
    secrets::store(REFRESH_TOKEN_ACCOUNT, &refresh_token).context("storing the refresh token")?;
    println!("Spotify ist angemeldet.");
    Ok(())
}

fn prompt_client_id() -> anyhow::Result<String> {
    print!("Spotify-Client-ID: ");
    io::stdout().flush().context("writing the prompt")?;
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .context("reading the client id")?;
    let client_id = line.trim();
    if client_id.is_empty() {
        bail!("no client id entered");
    }
    Ok(client_id.to_string())
}

/// Open `url` on its own thread: `xdg-open` may stay around as long as the browser does.
fn open_browser(url: String) {
    thread::spawn(move || {
        // The error may quote the URL, so it stays out of the log.
        if tools::run("xdg-open", &[&url]).is_err() {
            println!("Der Browser ließ sich nicht öffnen.");
        }
    });
}

/// Wait for the callback on the non-blocking `listener` and return the code.
fn wait_for_code(listener: &TcpListener, state: &str) -> anyhow::Result<String> {
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((stream, _)) => match answer(stream, state) {
                Callback::Ignored => {}
                Callback::Rejected => bail!("Spotify callback rejected"),
                Callback::Code(code) => return Ok(code),
            },
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL_INTERVAL),
            Err(err) => return Err(err).context("accepting the Spotify callback"),
        }
    }
    bail!(
        "no answer from Spotify within {} s",
        LOGIN_TIMEOUT.as_secs()
    )
}

/// Read one request from `stream`, classify it and answer the browser.
fn answer(mut stream: TcpStream, state: &str) -> Callback {
    let request_line = read_request_line(&stream).unwrap_or_default();
    let callback = classify(&request_line, state);
    let (status, body) = match callback {
        Callback::Ignored => ("404 Not Found", ""),
        Callback::Rejected => ("400 Bad Request", PAGE_REJECTED),
        Callback::Code(_) => ("200 OK", PAGE_DONE),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // The browser page is a courtesy; the login goes on without it.
    let _ = stream.write_all(response.as_bytes());
    callback
}

fn read_request_line(stream: &TcpStream) -> io::Result<String> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    let mut lines = BufReader::new(stream.take(MAX_REQUEST_BYTES)).lines();
    let request_line = lines.next().transpose()?.unwrap_or_default();
    // Closing with unread headers resets the connection and the browser loses the page.
    lines.map_while(Result::ok).find(String::is_empty);
    Ok(request_line)
}

/// URL-safe random string for the PKCE verifier and the state.
fn random_token() -> anyhow::Result<String> {
    let mut bytes = [0_u8; RANDOM_BYTES];
    File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .context("reading /dev/urandom")?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// What to start playing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// An artist, album or playlist, played through `context_uri`.
    Context { uri: String, name: String },
    /// A single track, played through `uris`.
    Track { uri: String, name: String },
}

/// `(uri, name)` of each entry in `container["items"]`; Spotify also sends `null` entries.
fn items(container: &serde_json::Value) -> impl Iterator<Item = (String, String)> + '_ {
    container["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some((
                item["uri"].as_str()?.to_string(),
                item["name"].as_str()?.to_string(),
            ))
        })
}

fn parse(json: &str) -> serde_json::Value {
    serde_json::from_str(json).unwrap_or_default()
}

/// Id of the active device, else the first one.
pub fn pick_device(json: &str) -> Option<String> {
    let json = parse(json);
    let devices = json["devices"].as_array()?;
    let id = |device: &serde_json::Value| device["id"].as_str().map(str::to_string);
    devices
        .iter()
        .find(|device| device["is_active"].as_bool() == Some(true))
        .and_then(id)
        .or_else(|| devices.first().and_then(id))
}

/// Artist if its name equals `query` ignoring case, else the first track.
pub fn pick_any(json: &str, query: &str) -> Option<Target> {
    let json = parse(json);
    if let Some((uri, name)) = items(&json["artists"]).next()
        && name.to_lowercase() == query.to_lowercase()
    {
        return Some(Target::Context { uri, name });
    }
    let (uri, name) = items(&json["tracks"]).next()?;
    Some(Target::Track { uri, name })
}

/// First of the user's playlists whose name contains `query` ignoring case.
pub fn pick_playlist(json: &str, query: &str) -> Option<Target> {
    let query = query.to_lowercase();
    let (uri, name) = items(&parse(json)).find(|(_, name)| name.to_lowercase().contains(&query))?;
    Some(Target::Context { uri, name })
}

/// First search hit for `kind`; `Any` has its own rule in [`pick_any`].
pub fn pick_first(json: &str, kind: ContentKind) -> Option<Target> {
    let json = parse(json);
    match kind {
        ContentKind::Any => None,
        ContentKind::Playlist => {
            let (uri, name) = items(&json["playlists"]).next()?;
            Some(Target::Context { uri, name })
        }
        ContentKind::Album => {
            let (uri, name) = items(&json["albums"]).next()?;
            Some(Target::Context { uri, name })
        }
        ContentKind::Track => {
            let (uri, name) = items(&json["tracks"]).next()?;
            Some(Target::Track { uri, name })
        }
    }
}

/// Body of `PUT /me/player/play`.
pub fn play_body(target: &Target) -> serde_json::Value {
    match target {
        Target::Context { uri, .. } => serde_json::json!({ "context_uri": uri }),
        Target::Track { uri, .. } => serde_json::json!({ "uris": [uri] }),
    }
}

/// The spoken sentence for the outcome of [`play`].
pub fn reply_for(result: Result<String, SpotifyError>) -> Reply {
    Reply::Say(match result {
        Ok(name) => format!("Ich spiele {name}."),
        Err(SpotifyError::NotLoggedIn) => "Spotify ist noch nicht angemeldet.".into(),
        Err(SpotifyError::NoDevice) => "Spotify läuft gerade nicht.".into(),
        Err(SpotifyError::NotFound) => "Dazu habe ich auf Spotify nichts gefunden.".into(),
        Err(SpotifyError::Unreachable) => "Spotify ist gerade nicht erreichbar.".into(),
    })
}

/// Search Spotify for `query` and start playing the best hit on the user's device.
pub fn play(kind: ContentKind, query: &str) -> Reply {
    reply_for(start_playback(kind, query))
}

/// Name of the content that started playing.
fn start_playback(kind: ContentKind, query: &str) -> Result<String, SpotifyError> {
    let token = access_token()?;
    let device = pick_device(&api_get(&token, &format!("{API_URL}/me/player/devices"))?)
        .ok_or(SpotifyError::NoDevice)?;
    let target = find_target(&token, kind, query)?;
    let name = match &target {
        Target::Context { name, .. } | Target::Track { name, .. } => name.clone(),
    };
    let url = format!(
        "{API_URL}/me/player/play?device_id={}",
        utf8_percent_encode(&device, QUERY_VALUE)
    );
    let mut response = tools::http_agent()
        .put(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .send(play_body(&target).to_string())
        .map_err(|err| {
            tracing::warn!(%err, "Spotify play request failed");
            SpotifyError::Unreachable
        })?;
    check_status(response.status().as_u16())?;
    // The answer is an empty 204; drain it so the connection ends cleanly.
    let _ = response.body_mut().read_to_string();
    Ok(name)
}

fn find_target(token: &str, kind: ContentKind, query: &str) -> Result<Target, SpotifyError> {
    let search = |types: &str| {
        api_get(
            token,
            &format!(
                "{API_URL}/search?type={types}&limit=1&q={}",
                utf8_percent_encode(query, QUERY_VALUE)
            ),
        )
    };
    let target = match kind {
        ContentKind::Any => pick_any(&search("artist,track")?, query),
        ContentKind::Playlist => {
            let mine = api_get(token, &format!("{API_URL}/me/playlists?limit=50"))?;
            match pick_playlist(&mine, query) {
                Some(found) => Some(found),
                None => pick_first(&search("playlist")?, kind),
            }
        }
        ContentKind::Album => pick_first(&search("album")?, kind),
        ContentKind::Track => pick_first(&search("track")?, kind),
    };
    target.ok_or(SpotifyError::NotFound)
}

/// 401 means the login is gone, anything else outside 2xx is a Spotify problem.
fn check_status(status: u16) -> Result<(), SpotifyError> {
    match status {
        200..=299 => Ok(()),
        401 => Err(SpotifyError::NotLoggedIn),
        _ => {
            tracing::warn!(status, "Spotify API returned an error");
            Err(SpotifyError::Unreachable)
        }
    }
}

fn api_get(token: &str, url: &str) -> Result<String, SpotifyError> {
    let mut response = tools::http_agent()
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .call()
        .map_err(|err| {
            tracing::warn!(%err, "Spotify API request failed");
            SpotifyError::Unreachable
        })?;
    check_status(response.status().as_u16())?;
    response.body_mut().read_to_string().map_err(|err| {
        tracing::warn!(%err, "Spotify API response unreadable");
        SpotifyError::Unreachable
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_matches_rfc_7636_appendix_b() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn authorize_url_carries_all_parameters() {
        let url = authorize_url("client id", "chal", "st");
        assert!(
            url.starts_with("https://accounts.spotify.com/authorize?"),
            "{url}"
        );
        for part in [
            "response_type=code",
            "client_id=client%20id",
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A8898%2Fcallback",
            "scope=user-modify-playback-state%20user-read-playback-state%20playlist-read-private",
            "code_challenge_method=S256",
            "code_challenge=chal",
            "state=st",
        ] {
            assert!(url.contains(part), "{part} missing in {url}");
        }
    }

    #[test]
    fn callback_with_matching_state_yields_the_code() {
        assert_eq!(
            parse_callback("GET /callback?code=abc&state=s HTTP/1.1", "s"),
            Some("abc".into())
        );
        assert_eq!(
            parse_callback("GET /callback?state=s&code=abc HTTP/1.1", "s"),
            Some("abc".into())
        );
    }

    #[test]
    fn callback_decodes_the_code() {
        assert_eq!(
            parse_callback("GET /callback?code=a%2Fb&state=s HTTP/1.1", "s"),
            Some("a/b".into())
        );
    }

    #[test]
    fn callback_with_wrong_or_missing_state_is_rejected() {
        assert_eq!(
            parse_callback("GET /callback?code=abc&state=x HTTP/1.1", "s"),
            None
        );
        assert_eq!(parse_callback("GET /callback?code=abc HTTP/1.1", "s"), None);
    }

    #[test]
    fn callback_with_error_is_rejected() {
        assert_eq!(
            parse_callback("GET /callback?error=access_denied&state=s HTTP/1.1", "s"),
            None
        );
        assert_eq!(
            parse_callback("GET /callback?code=abc&error=x&state=s HTTP/1.1", "s"),
            None
        );
    }

    #[test]
    fn other_requests_yield_no_code() {
        assert_eq!(
            parse_callback("GET /favicon.ico?code=abc&state=s HTTP/1.1", "s"),
            None
        );
        assert_eq!(
            parse_callback("POST /callback?code=abc&state=s HTTP/1.1", "s"),
            None
        );
        assert_eq!(parse_callback("", "s"), None);
    }

    #[test]
    fn classify_ignores_everything_but_the_callback_path() {
        assert_eq!(
            classify("GET /favicon.ico HTTP/1.1", "s"),
            Callback::Ignored
        );
        assert_eq!(classify("GET /callback/x HTTP/1.1", "s"), Callback::Ignored);
        assert_eq!(classify("", "s"), Callback::Ignored);
    }

    #[test]
    fn classify_rejects_a_bad_callback() {
        assert_eq!(classify("GET /callback HTTP/1.1", "s"), Callback::Rejected);
        assert_eq!(
            classify("GET /callback?error=access_denied&state=s HTTP/1.1", "s"),
            Callback::Rejected
        );
    }

    #[test]
    fn classify_accepts_a_good_callback() {
        assert_eq!(
            classify("GET /callback?code=abc&state=s HTTP/1.1", "s"),
            Callback::Code("abc".into())
        );
    }

    #[test]
    fn token_response_with_refresh_token() {
        let tokens = parse_token(200, r#"{"access_token":"a","refresh_token":"r"}"#).unwrap();
        assert_eq!(tokens.access, "a");
        assert_eq!(tokens.refresh.as_deref(), Some("r"));
    }

    #[test]
    fn token_response_without_refresh_token() {
        let tokens = parse_token(200, r#"{"access_token":"a"}"#).unwrap();
        assert_eq!(tokens.refresh, None);
    }

    #[test]
    fn invalid_grant_means_not_logged_in() {
        assert!(matches!(
            parse_token(400, r#"{"error":"invalid_grant"}"#),
            Err(SpotifyError::NotLoggedIn)
        ));
    }

    #[test]
    fn other_failures_mean_unreachable() {
        for (status, body) in [
            (503, ""),
            (400, r#"{"error":"invalid_request"}"#),
            (400, "kein JSON"),
            (200, "kein JSON"),
            (200, r#"{"token_type":"Bearer"}"#),
        ] {
            assert!(
                matches!(parse_token(status, body), Err(SpotifyError::Unreachable)),
                "{status} {body}"
            );
        }
    }

    #[test]
    fn tokens_debug_hides_the_values() {
        let tokens = parse_token(200, r#"{"access_token":"a1b2","refresh_token":"c3d4"}"#).unwrap();
        let text = format!("{tokens:?}");
        assert!(!text.contains("a1b2") && !text.contains("c3d4"), "{text}");
    }

    #[test]
    fn stored_value_keeps_a_value() {
        assert_eq!(stored_value(Ok(Some("v".into()))), Ok("v".into()));
    }

    #[test]
    fn missing_entry_means_not_logged_in() {
        assert_eq!(stored_value(Ok(None)), Err(SpotifyError::NotLoggedIn));
    }

    #[test]
    fn failed_read_means_unreachable() {
        assert_eq!(
            stored_value(Err(anyhow::anyhow!("wallet locked"))),
            Err(SpotifyError::Unreachable)
        );
    }

    #[test]
    fn random_tokens_are_long_url_safe_and_differ() {
        let first = random_token().unwrap();
        assert_eq!(first.len(), 43);
        assert!(
            first
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert_ne!(first, random_token().unwrap());
    }

    fn ctx(uri: &str, name: &str) -> Target {
        Target::Context {
            uri: uri.into(),
            name: name.into(),
        }
    }

    const BOTH: &str = r#"{
        "artists":{"items":[{"uri":"spotify:artist:1","name":"Daft Punk"}]},
        "tracks":{"items":[{"uri":"spotify:track:2","name":"Around the World"}]}}"#;

    #[test]
    fn active_device_wins() {
        let json = r#"{"devices":[{"id":"a","is_active":false},{"id":"b","is_active":true}]}"#;
        assert_eq!(pick_device(json), Some("b".into()));
    }

    #[test]
    fn first_device_without_active_one() {
        let json = r#"{"devices":[{"id":"a","is_active":false}]}"#;
        assert_eq!(pick_device(json), Some("a".into()));
    }

    #[test]
    fn no_devices_yield_none() {
        assert_eq!(pick_device(r#"{"devices":[]}"#), None);
        assert_eq!(pick_device("kein JSON"), None);
    }

    #[test]
    fn any_prefers_the_artist_with_the_same_name() {
        assert_eq!(
            pick_any(BOTH, "daft punk"),
            Some(ctx("spotify:artist:1", "Daft Punk"))
        );
    }

    #[test]
    fn any_falls_back_to_the_track() {
        assert_eq!(
            pick_any(BOTH, "around the world"),
            Some(Target::Track {
                uri: "spotify:track:2".into(),
                name: "Around the World".into()
            })
        );
    }

    #[test]
    fn any_without_hits_yields_none() {
        assert_eq!(
            pick_any(r#"{"artists":{"items":[]},"tracks":{"items":[]}}"#, "x"),
            None
        );
    }

    const MINE: &str =
        r#"{"items":[null,{"uri":"u1","name":"Mein Sport"},{"uri":"u2","name":"Ruhe"}]}"#;

    #[test]
    fn playlist_name_contains_the_query() {
        assert_eq!(pick_playlist(MINE, "sport"), Some(ctx("u1", "Mein Sport")));
    }

    #[test]
    fn playlist_without_match_yields_none() {
        assert_eq!(pick_playlist(MINE, "jazz"), None);
    }

    #[test]
    fn first_skips_null_items() {
        assert_eq!(
            pick_first(
                r#"{"playlists":{"items":[null,{"uri":"u","name":"n"}]}}"#,
                ContentKind::Playlist
            ),
            Some(ctx("u", "n"))
        );
    }

    #[test]
    fn first_reads_the_section_of_its_kind() {
        let json = r#"{"albums":{"items":[{"uri":"a","name":"A"}]},"tracks":{"items":[{"uri":"t","name":"T"}]}}"#;
        assert_eq!(pick_first(json, ContentKind::Album), Some(ctx("a", "A")));
        assert_eq!(
            pick_first(json, ContentKind::Track),
            Some(Target::Track {
                uri: "t".into(),
                name: "T".into()
            })
        );
        assert_eq!(pick_first("{}", ContentKind::Track), None);
    }

    #[test]
    fn play_bodies() {
        assert_eq!(
            play_body(&ctx("u", "n")),
            serde_json::json!({"context_uri": "u"})
        );
        assert_eq!(
            play_body(&Target::Track {
                uri: "u".into(),
                name: "n".into()
            }),
            serde_json::json!({"uris": ["u"]})
        );
    }

    #[test]
    fn replies_are_the_spec_sentences() {
        let say = |text: &str| Reply::Say(text.into());
        assert_eq!(
            reply_for(Ok("Daft Punk".into())),
            say("Ich spiele Daft Punk.")
        );
        assert_eq!(
            reply_for(Err(SpotifyError::NotLoggedIn)),
            say("Spotify ist noch nicht angemeldet.")
        );
        assert_eq!(
            reply_for(Err(SpotifyError::NoDevice)),
            say("Spotify läuft gerade nicht.")
        );
        assert_eq!(
            reply_for(Err(SpotifyError::NotFound)),
            say("Dazu habe ich auf Spotify nichts gefunden.")
        );
        assert_eq!(
            reply_for(Err(SpotifyError::Unreachable)),
            say("Spotify ist gerade nicht erreichbar.")
        );
    }
}

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

use crate::{secrets, tools};

const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
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
/// [`SpotifyError::NotLoggedIn`] without usable entries in KWallet or after a revoked
/// grant, [`SpotifyError::Unreachable`] when the token request fails.
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
        tracing::warn!(%err, "storing the new Spotify refresh token failed");
    }
    Ok(tokens.access)
}

fn stored(account: &str) -> Result<String, SpotifyError> {
    secrets::read(account)
        .inspect_err(|err| tracing::warn!(%err, account, "reading from KWallet failed"))
        .ok()
        .flatten()
        .ok_or(SpotifyError::NotLoggedIn)
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
}

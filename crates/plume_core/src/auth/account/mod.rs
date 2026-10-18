mod login;
mod token;
mod two_factor_auth;

use aes::cipher::BlockModeDecrypt;
use cbc::cipher::{KeyIvInit, block_padding::Pkcs7};
use hmac::{Hmac, KeyInit, Mac};
use reqwest::Response;
use sha2::Sha256;
use srp::ClientVerifier;

use crate::Error;

/// Parses a GrandSlam reply, logging enough to tell where a sign-in dies.
///
/// `step` names the request ("SRP init", "SRP complete", "apptokens") so the
/// log shows exactly which of the three sign-in requests Apple refused. On a
/// refusal the status, every response header (bar cookies) and the start of
/// the body are logged: an Apple error page usually says why, and the edge
/// headers show whether the answer came from Apple or from something in front
/// of it. A parsed plist is never logged -- it carries the session tokens.
pub async fn parse_response(
    step: &str,
    res: Result<Response, reqwest::Error>,
) -> Result<plist::Dictionary, Error> {
    let res = match res {
        Ok(res) => res,
        Err(e) => {
            log::error!("[gsa {step}] no response at all: {e}");
            return Err(e.into());
        }
    };

    let status = res.status();
    let headers = describe_headers(res.headers());
    let retry_after = retry_after(res.headers());
    let body = res.text().await?;

    // Apple answers a good request with an XML plist. When it is rate limiting
    // -- which happens easily after several sign-in attempts from one machine or
    // IP -- or is having a transient hiccup, it answers with an HTML error page
    // or a 5xx instead. Parsing that as a plist produced the baffling
    // `invalid type: string "<html>", expected a map` error at sign-in. Flag it
    // as busy so the caller can wait and retry rather than surface the noise.
    let dict: plist::Dictionary = match plist::from_bytes(body.as_bytes()) {
        Ok(dict) => {
            log::info!("[gsa {step}] HTTP {status}, plist OK ({} bytes)", body.len());
            dict
        }
        Err(parse_err) => {
            let snippet: String = body.chars().take(500).collect();
            log::error!(
                "[gsa {step}] HTTP {status}, body is not a plist.\n  headers: {headers}\n  body starts: {snippet:?}"
            );

            // 429 is a rate limit: the caller should wait, for as long as
            // Apple says if it says. Anything else that is not a plist -- a
            // 5xx, or an HTML page on any status -- is a refusal worth at most
            // one quick retry.
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(Error::RateLimited {
                    detail: format!("{step}: HTTP {status}"),
                    retry_after,
                });
            }

            let transient = status.is_server_error() || body.trim_start().starts_with('<');

            return if transient {
                Err(Error::AppleBusy(format!("{step}: HTTP {status}")))
            } else {
                Err(Error::Plist(parse_err))
            };
        }
    };

    match dict.get("Response") {
        Some(plist::Value::Dictionary(inner)) => Ok(inner.clone()),
        _ => {
            log::error!("[gsa {step}] plist has no `Response` dictionary");
            Err(crate::Error::Parse)
        }
    }
}

/// The `Retry-After` header as a duration, when Apple sends one in seconds.
///
/// The HTTP-date form is not handled; Apple's edge has only been seen sending
/// seconds, and a missing value simply means the caller picks its own wait.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(std::time::Duration::from_secs)
}

/// Response headers as one line, minus cookies (which can carry session state).
fn describe_headers(headers: &reqwest::header::HeaderMap) -> String {
    headers
        .iter()
        .filter(|(name, _)| !name.as_str().eq_ignore_ascii_case("set-cookie"))
        .map(|(name, value)| format!("{name}={}", value.to_str().unwrap_or("<binary>")))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn check_error(res: &plist::Dictionary) -> Result<(), Error> {
    let res = match res.get("Status") {
        Some(plist::Value::Dictionary(d)) => d,
        _ => res,
    };

    // No `ec` field means there is no error status to read -- some responses
    // simply do not carry one. Reading it with `.unwrap()` turned that into a
    // panic, and a panic here kills the login thread and leaves the window
    // stuck on "verifying" with no message. Treat a missing code as success.
    let Some(ec) = res.get("ec").and_then(plist::Value::as_signed_integer) else {
        return Ok(());
    };

    if ec != 0 {
        let message = res
            .get("em")
            .and_then(plist::Value::as_string)
            .unwrap_or("unknown error")
            .to_owned();
        log::warn!("[gsa] Apple returned error ec={ec}: {message}");
        return Err(Error::AuthSrpWithMessage(ec, message));
    }

    Ok(())
}

pub fn decrypt_cbc(usr: &ClientVerifier<Sha256>, data: &[u8]) -> Vec<u8> {
    let extra_data_key = create_session_key(usr, "extra data key:");
    let extra_data_iv = create_session_key(usr, "extra data iv:");
    let extra_data_iv = &extra_data_iv[..16];

    cbc::Decryptor::<aes::Aes256>::new_from_slices(&extra_data_key, extra_data_iv)
        .unwrap()
        .decrypt_padded_vec::<Pkcs7>(&data)
        .unwrap()
}

pub fn create_session_key(usr: &ClientVerifier<Sha256>, name: &str) -> Vec<u8> {
    Hmac::<Sha256>::new_from_slice(&usr.key())
        .unwrap()
        .chain_update(name.as_bytes())
        .finalize()
        .into_bytes()
        .to_vec()
}

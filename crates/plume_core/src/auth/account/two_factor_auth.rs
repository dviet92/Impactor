use std::str::FromStr;
use std::time::Duration;

use crate::Error;
use base64::{Engine, engine::general_purpose};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use crate::auth::{Account, AuthenticationExtras, LoginState, PhoneNumber, VerifyBody, VerifyCode};

impl Account {
    pub async fn send_2fa_to_devices(&self) -> Result<LoginState, Error> {
        let headers = self.build_2fa_headers(false).await;

        let res = self
            .client
            .get("https://gsa.apple.com/auth/verify/trusteddevice")
            .headers(headers)
            .send()
            .await?;

        let status_code = res.status();

        if !status_code.is_success() {
            // Log what came back: an HTML page here says whether it is the same
            // edge refusal as sign-in or a different rejection.
            let snippet: String = res
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(300)
                .collect();
            log::error!(
                "[gsa 2FA push] HTTP {status_code}; body starts: {snippet:?}"
            );
            return Err(Error::AuthSrpWithMessage(
                status_code.as_u16() as i64,
                "Failed to send 2FA to devices".to_string(),
            ));
        }

        log::info!("[gsa 2FA push] HTTP {status_code}, code requested on trusted devices");
        return Ok(LoginState::Needs2FAVerification);
    }

    pub async fn send_sms_2fa_to_devices(&self, phone_id: u32) -> Result<LoginState, Error> {
        let headers = self.build_2fa_headers(true).await;

        let body = VerifyBody {
            phone_number: PhoneNumber { id: phone_id },
            mode: "sms".to_string(),
            security_code: None,
        };

        let res = self
            .client
            .put("https://gsa.apple.com/auth/verify/phone")
            .headers(headers)
            .json(&body)
            .send()
            .await?;

        let status_code = res.status();

        if !status_code.is_success() {
            return Err(Error::AuthSrpWithMessage(
                status_code.as_u16() as i64,
                "Failed to send SMS 2FA to devices".to_string(),
            ));
        }

        return Ok(LoginState::NeedsSMS2FAVerification(body));
    }

    pub async fn get_auth_extras(&self) -> Result<AuthenticationExtras, Error> {
        let headers = self.build_2fa_headers(true);

        let req = self
            .client
            .get("https://gsa.apple.com/auth")
            .headers(headers.await)
            .header("Accept", "application/json")
            .send()
            .await?;
        let status = req.status().as_u16();
        let mut new_state = req.json::<AuthenticationExtras>().await?;
        if status == 201 {
            new_state.new_state = Some(LoginState::NeedsSMS2FAVerification(VerifyBody {
                phone_number: PhoneNumber {
                    id: new_state.trusted_phone_numbers.first().unwrap().id,
                },
                mode: "sms".to_string(),
                security_code: None,
            }));
        }

        Ok(new_state)
    }

    pub async fn verify_2fa(&self, code: String) -> Result<LoginState, Error> {
        // Apple answers a valid request with a plist. When it is rate limiting
        // -- which happens easily after signing several accounts from one
        // machine -- or is having a transient hiccup, it answers with an HTML
        // error page or a 5xx instead. That used to be parsed as a plist and
        // surfaced as `invalid type: string "<html>", expected a map`, an error
        // that looks fatal but is not: the trusted-device code stays valid for
        // a few minutes and allows several attempts, so waiting and retrying
        // the same code usually gets through.
        const ATTEMPTS: u32 = 4;
        const BACKOFF: Duration = Duration::from_secs(2);
        // A 429 here is the same sign-in rate limit as at SRP init; two seconds
        // later only adds to the count. The trusted-device code stays valid for
        // a few minutes, so a longer wait costs nothing but patience.
        // The user's 429 wait, but never more than 20s per step here: the
        // code they typed is only good for a few minutes.
        let rate_limit_backoff = crate::net::rate_limit_wait().min(Duration::from_secs(20));

        let mut last_transient: Option<String> = None;
        let mut next_wait = BACKOFF;

        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(next_wait * attempt).await;
            }

            let headers = self.build_2fa_headers(false).await;
            let res = self
                .client
                .get("https://gsa.apple.com/grandslam/GsService2/validate")
                .headers(headers)
                .header(
                    HeaderName::from_str("security-code").unwrap(),
                    HeaderValue::from_str(&code).unwrap(),
                )
                .send()
                .await?;

            let status = res.status();
            let body = res.text().await?;

            match plist::from_bytes::<plist::Dictionary>(body.as_bytes()) {
                Ok(dict) => {
                    // A parsed plist carrying a non-zero error code is a real
                    // answer (a wrong or expired code), not a transient one, so
                    // it is returned rather than retried.
                    super::check_error(&dict)?;
                    return Ok(LoginState::NeedsLogin);
                }
                Err(parse_err) => {
                    let transient = status.is_server_error()
                        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
                        || body.trim_start().starts_with('<');

                    if !transient {
                        return Err(Error::Plist(parse_err));
                    }

                    next_wait = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                        rate_limit_backoff
                    } else {
                        BACKOFF
                    };
                    log::warn!(
                        "2FA validation got a non-plist response (HTTP {status}); \
                         attempt {}/{ATTEMPTS}, retrying in {}s",
                        attempt + 1,
                        (next_wait * (attempt + 1)).as_secs()
                    );
                    last_transient = Some(format!("HTTP {status}"));
                }
            }
        }

        Err(Error::AuthSrpWithMessage(
            -1,
            format!(
                "Apple's verification service is busy ({}). Wait a moment and enter the code again.",
                last_transient.as_deref().unwrap_or("temporary error")
            ),
        ))
    }

    pub async fn verify_sms_2fa(
        &self,
        code: String,
        mut body: VerifyBody,
    ) -> Result<LoginState, Error> {
        let headers = self.build_2fa_headers(true).await;
        body.security_code = Some(VerifyCode { code });
        let res = self
            .client
            .post("https://gsa.apple.com/auth/verify/phone/securitycode")
            .headers(headers)
            .json(&body)
            .send()
            .await?;

        let status_code = res.status();

        // TODO: 423 http code may occur, in this case we to ask for sending
        // last code sent (unlikely it would even work), or try again later
        if !status_code.is_success() {
            return Err(Error::Bad2faCode);
        }

        Ok(LoginState::NeedsLogin)
    }

    async fn build_2fa_headers(&self, sms: bool) -> HeaderMap {
        let spd = self.spd.as_ref().unwrap();
        let dsid = spd.get("adsid").unwrap().as_string().unwrap();
        let token = spd.get("GsIdmsToken").unwrap().as_string().unwrap();

        let identity_token = general_purpose::STANDARD.encode(format!("{}:{}", dsid, token));

        let mut headers = HeaderMap::new();
        let valid_anisette = self.get_anisette().await;
        for (k, v) in valid_anisette.generate_headers(false, true, true) {
            headers.insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(&v).unwrap(),
            );
        }
        if !sms {
            headers.insert("Content-Type", HeaderValue::from_static("text/x-xml-plist"));
            headers.insert("Accept", HeaderValue::from_static("text/x-xml-plist"));
        } else {
            headers.insert("Content-Type", HeaderValue::from_static("application/json"));
            headers.insert("Accept", HeaderValue::from_static("application/json"));
        }
        headers.insert("User-Agent", HeaderValue::from_static("Xcode"));
        headers.insert("Accept-Language", HeaderValue::from_static("en-us"));
        headers.append(
            "X-Apple-Identity-Token",
            HeaderValue::from_str(&identity_token).unwrap(),
        );

        if let Ok(locale) = valid_anisette.get_header("x-apple-locale") {
            headers.insert("Loc", HeaderValue::from_str(&locale).unwrap());
        }

        headers
    }
}

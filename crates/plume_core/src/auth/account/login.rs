use std::time::Duration;

use omnisette::AnisetteConfiguration;
use plist::{Dictionary, Value};
use reqwest::header::{HeaderMap, HeaderValue};
use sha2::{Digest, Sha256};
use srp::groups::G2048;

use crate::Error;

use crate::auth::account::{check_error, parse_response};
use crate::auth::anisette_data::AnisetteData;
use crate::auth::{
    Account, ChallengeRequest, ChallengeRequestBody, GSA_ENDPOINT, GSA_USER_AGENT, InitRequest,
    InitRequestBody,
    LoginState, RequestHeader, TrustedPhoneNumber, TwoFactorAction, TwoFactorMethod,
    TwoFactorRequest,
};

#[macro_export]
macro_rules! plist_get_string {
    ($base:expr, $( $path:literal )+, $final_key:literal) => {{
        let mut current_val = $base;
        $(
            current_val = current_val
                .get($path)
                .expect(concat!("Missing dictionary key: ", $path))
                .as_dictionary()
                .expect(concat!("Key value is not a dictionary: ", $path));
        )+
        current_val
            .get($final_key)
            .expect(concat!("Missing string key: ", $final_key))
            .as_string()
            .expect(concat!("Value is not a string: ", $final_key))
            .to_string()
    }};

    ($base:expr, $key:literal) => {{
        $base
            .get($key)
            .expect(concat!("Missing key: ", $key))
            .as_string()
            .expect(concat!("Value is not a string: ", $key))
            .to_string()
    }};
}

/// `dviet40@icloud.com` -> `dv***@icloud.com`, so a log can be shared without
/// handing over the account name.
fn redact_email(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => {
            let shown: String = local.chars().take(2).collect();
            format!("{shown}***@{domain}")
        }
        None => "***".to_string(),
    }
}

impl Account {
    pub async fn login(
        appleid_closure: impl Fn() -> Result<(String, String), String>,
        tfa_closure: impl Fn(TwoFactorRequest) -> Result<TwoFactorAction, String>,
        config: AnisetteConfiguration,
    ) -> Result<Account, Error> {
        let anisette = AnisetteData::new(config).await?;
        Account::login_with_anisette(appleid_closure, tfa_closure, anisette).await
    }

    pub async fn login_with_anisette<
        F: Fn() -> Result<(String, String), String>,
        G: Fn(TwoFactorRequest) -> Result<TwoFactorAction, String>,
    >(
        appleid_closure: F,
        tfa_closure: G,
        anisette: AnisetteData,
    ) -> Result<Account, Error> {
        let mut _self = Account::new_with_anisette(anisette)?;
        let (username, password) = appleid_closure().map_err(|e| {
            Error::AuthSrpWithMessage(0, format!("Failed to get Apple ID credentials: {}", e))
        })?;

        let mut response = _self.login_email_pass_retrying(&username, &password).await?;
        // Cached so the caller can be offered an SMS fallback even when a trusted
        // device handled the push. Fetched lazily the first time 2FA is needed.
        let mut trusted_phone_numbers: Vec<TrustedPhoneNumber> = Vec::new();

        loop {
            match response {
                LoginState::NeedsDevice2FA => {
                    log::info!("[gsa] password accepted; asking Apple to push a 2FA code to trusted devices");
                    response = _self.send_2fa_to_devices().await?;
                    if trusted_phone_numbers.is_empty() {
                        if let Ok(extras) = _self.get_auth_extras().await {
                            trusted_phone_numbers = extras.trusted_phone_numbers;
                        }
                    }
                }
                LoginState::Needs2FAVerification => {
                    log::info!("[gsa] 2FA push sent; waiting for the code to be entered");
                    let request = TwoFactorRequest {
                        method: TwoFactorMethod::Device,
                        trusted_phone_numbers: trusted_phone_numbers.clone(),
                    };
                    match tfa_closure(request).map_err(|e| {
                        Error::AuthSrpWithMessage(0, format!("Failed to get 2FA code: {}", e))
                    })? {
                        TwoFactorAction::SubmitCode(code) => {
                            response = _self.verify_2fa(code).await?
                        }
                        TwoFactorAction::SendSms(id) => {
                            response = _self.send_sms_2fa_to_devices(id).await?
                        }
                    }
                }
                LoginState::NeedsSMS2FA => {
                    log::info!("[gsa] Apple wants SMS 2FA");
                    if trusted_phone_numbers.is_empty() {
                        if let Ok(extras) = _self.get_auth_extras().await {
                            trusted_phone_numbers = extras.trusted_phone_numbers;
                        }
                    }
                    let id = trusted_phone_numbers.first().map(|p| p.id).unwrap_or(1);
                    response = _self.send_sms_2fa_to_devices(id).await?;
                }
                LoginState::NeedsSMS2FAVerification(body) => {
                    let request = TwoFactorRequest {
                        method: TwoFactorMethod::Sms,
                        trusted_phone_numbers: trusted_phone_numbers.clone(),
                    };
                    match tfa_closure(request).map_err(|e| {
                        Error::AuthSrpWithMessage(
                            0,
                            format!("Failed to get SMS 2FA code: {}", e),
                        )
                    })? {
                        TwoFactorAction::SubmitCode(code) => {
                            response = _self.verify_sms_2fa(code, body).await?
                        }
                        TwoFactorAction::SendSms(id) => {
                            response = _self.send_sms_2fa_to_devices(id).await?
                        }
                    }
                }
                LoginState::NeedsLogin => {
                    log::info!("[gsa] 2FA verified; signing in again to obtain the session");
                    response = _self.login_email_pass_retrying(&username, &password).await?
                }
                LoginState::LoggedIn => {
                    log::info!("[gsa] signed in");
                    return Ok(_self);
                }
                LoginState::NeedsExtraStep(step) => {
                    log::warn!("[gsa] Apple asks for an extra step: {step}");
                    if _self.get_pet().is_some() {
                        return Ok(_self);
                    } else {
                        return Err(Error::ExtraStep(step));
                    }
                }
            }
        }
    }

    /// Signs in, retrying when Apple answers with a busy page or a rate limit.
    ///
    /// The whole exchange is retried, not just the failed request: each attempt
    /// recomputes a fresh SRP ephemeral and anisette, so there is no half-used
    /// state to carry over. A real credential or protocol error is returned at
    /// once. Two kinds of refusal are retried, differently:
    ///
    /// * [`Error::AppleBusy`] -- a 5xx or an HTML page on some other status.
    ///   One quick retry only. Every request rides its own connection, so this
    ///   is not the per-connection limit; it is Apple's edge refusing this IP or
    ///   device identity, and hammering it only extends the block.
    /// * [`Error::RateLimited`] -- HTTP 429. Apple counts sign-in requests per
    ///   network and identity over a short window, and several Apple IDs signed
    ///   in within a minute or two trip it. The account is fine and so is the
    ///   identity; the request just came too soon. Wait a real interval (what
    ///   `Retry-After` says, or a growing default) and try again, twice. Five
    ///   seconds later is not a retry, it is another hit on the counter.
    async fn login_email_pass_retrying(
        &mut self,
        username: &str,
        password: &str,
    ) -> Result<LoginState, Error> {
        const BUSY_BACKOFF: Duration = Duration::from_secs(5);
        /// Never sleep longer than this on Apple's say-so.
        const RATE_LIMIT_WAIT_CAP: Duration = Duration::from_secs(300);

        // Minimum waits before the second and third attempt after a 429. The
        // base is the user's: someone routing through a proxy pool can set it
        // near zero, since each attempt may leave from a different address.
        let base = crate::net::rate_limit_wait();
        let rate_limit_waits: [Duration; 2] = [base, base * 2];

        let mut busy_retried = false;
        let mut rate_limit_hits = 0usize;

        loop {
            match self.login_email_pass(username, password).await {
                Err(Error::AppleBusy(detail)) => {
                    if busy_retried {
                        return Err(Error::AuthSrpWithMessage(
                            -1,
                            format!(
                                "Apple is refusing sign-in from this network or device identity \
                                 ({detail}). This is not a wrong password. Try from a different \
                                 network (for example a phone hotspot), or wait several hours \
                                 before trying again -- every retry extends the block."
                            ),
                        ));
                    }
                    log::warn!(
                        "GSA login refused ({detail}); retrying once in {}s",
                        BUSY_BACKOFF.as_secs()
                    );
                    busy_retried = true;
                    tokio::time::sleep(BUSY_BACKOFF).await;
                }
                Err(Error::RateLimited {
                    detail,
                    retry_after,
                }) => {
                    let Some(minimum) = rate_limit_waits.get(rate_limit_hits) else {
                        return Err(Error::AuthSrpWithMessage(
                            -1,
                            format!(
                                "Apple is rate limiting sign-ins from this network ({detail}). \
                                 This is not a wrong password and the account is not locked: \
                                 too many Apple IDs signed in close together. Wait a few minutes \
                                 before the next one."
                            ),
                        ));
                    };
                    rate_limit_hits += 1;

                    let wait = retry_after
                        .unwrap_or(Duration::ZERO)
                        .max(*minimum)
                        .min(RATE_LIMIT_WAIT_CAP);
                    log::warn!(
                        "GSA rate limited ({detail}); waiting {}s before attempt {} of {}{}",
                        wait.as_secs(),
                        rate_limit_hits + 1,
                        rate_limit_waits.len() + 1,
                        match retry_after {
                            Some(hint) => format!(" (Apple asked for {}s)", hint.as_secs()),
                            None => String::new(),
                        }
                    );
                    tokio::time::sleep(wait).await;
                }
                other => return other,
            }
        }
    }

    pub async fn login_email_pass(
        &mut self,
        username: &str,
        password: &str,
    ) -> Result<LoginState, Error> {
        let username_for_spd = username.to_string().to_lowercase();
        let srp_client = srp::Client::<G2048, Sha256>::new_with_options(false);
        let a: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
        let a_pub = srp_client.compute_public_ephemeral(&a);

        let anisette = self.get_anisette().await;

        let mut gsa_headers = HeaderMap::new();
        gsa_headers.insert(
            "Content-Type",
            HeaderValue::from_str("text/x-xml-plist").unwrap(),
        );
        gsa_headers.insert("Accept", HeaderValue::from_str("*/*").unwrap());
        gsa_headers.insert(
            "User-Agent",
            HeaderValue::from_str(GSA_USER_AGENT).unwrap(),
        );
        gsa_headers.insert(
            "X-MMe-Client-Info",
            HeaderValue::from_str(&anisette.get_header("x-mme-client-info")?).unwrap(),
        );

        // What this attempt presents to Apple. The user agent is the field Apple
        // has been filtering on; the anisette machine token (X-Apple-I-MD-M)
        // and device id show whether provisioning actually produced an identity
        // -- a request without them is refused before credentials are checked.
        // Values that identify the machine are logged by presence only.
        log::info!(
            "[gsa SRP init] user={} ua={:?} client-info={:?} anisette: md-m={} device-id={} md={}",
            redact_email(&username_for_spd),
            GSA_USER_AGENT,
            anisette
                .get_header("x-mme-client-info")
                .unwrap_or_else(|_| "<missing>".to_string()),
            anisette.get_header("x-apple-i-md-m").is_ok(),
            anisette.get_header("x-mme-device-id").is_ok(),
            anisette.get_header("x-apple-i-md").is_ok(),
        );

        let header = RequestHeader {
            version: "1.0.1".to_string(),
        };
        let init_body = InitRequestBody {
            a_pub: plist::Value::Data(a_pub),
            cpd: anisette.to_plist(true, false, false),
            operation: "init".to_string(),
            ps: vec!["s2k".to_string(), "s2k_fo".to_string()],
            username: username_for_spd.clone(),
        };

        let init_packet = InitRequest {
            header: header.clone(),
            request: init_body,
        };

        let mut buffer = Vec::new();
        plist::to_writer_xml(&mut buffer, &init_packet)?;

        let res = self
            .client
            .post(GSA_ENDPOINT)
            .headers(gsa_headers.clone())
            .body(buffer)
            .send()
            .await;

        let res = parse_response("SRP init", res).await?;
        check_error(&res)?;

        let salt = res.get("s").unwrap().as_data().unwrap();
        let b_pub = res.get("B").unwrap().as_data().unwrap();
        let iters = res.get("i").unwrap().as_signed_integer().unwrap();
        let c = res.get("c").unwrap().as_string().unwrap();

        let hashed_password = Sha256::digest(password.as_bytes());

        let mut password_buf = [0u8; 32];
        pbkdf2::pbkdf2::<hmac::Hmac<Sha256>>(
            &hashed_password,
            salt,
            iters as u32,
            &mut password_buf,
        )?;

        let verifier = srp_client
            .process_reply(&a, username_for_spd.as_bytes(), &password_buf, salt, b_pub)
            .unwrap();

        let challenge_body = ChallengeRequestBody {
            m: plist::Value::Data(verifier.proof().to_vec()),
            c: c.to_string(),
            cpd: anisette.to_plist(true, false, false),
            operation: "complete".to_string(),
            username: username_for_spd.clone(),
        };

        let challenge_packet = ChallengeRequest {
            header,
            request: challenge_body,
        };

        let mut buffer = Vec::new();
        plist::to_writer_xml(&mut buffer, &challenge_packet)?;

        gsa_headers.insert("Connection", HeaderValue::from_static("close"));

        let res = self
            .client
            .post(GSA_ENDPOINT)
            .headers(gsa_headers)
            .body(buffer)
            .send()
            .await;

        let res = parse_response("SRP complete", res).await?;
        check_error(&res)?;

        let m2 = res.get("M2").unwrap().as_data().unwrap();
        verifier.verify_server(m2).unwrap();

        let spd_encrypted = res.get("spd").unwrap().as_data().unwrap();
        let spd_decrypted = super::decrypt_cbc(&verifier, spd_encrypted);
        let mut spd: Dictionary = plist::from_bytes(&spd_decrypted).unwrap();

        if !spd.contains_key("appleId") {
            spd.insert(
                "appleId".to_string(),
                plist::Value::String(username_for_spd),
            );
        }

        self.spd = Some(spd);

        let status = res.get("Status").unwrap().as_dictionary().unwrap();
        if let Some(Value::String(auth_type)) = status.get("au") {
            return match auth_type.as_str() {
                "trustedDeviceSecondaryAuth" => Ok(LoginState::NeedsDevice2FA),
                "secondaryAuth" => Ok(LoginState::NeedsSMS2FA),
                other => Ok(LoginState::NeedsExtraStep(other.to_string())),
            };
        }

        Ok(LoginState::LoggedIn)
    }

    pub fn get_pet(&self) -> Option<String> {
        let base = self.spd.as_ref().unwrap();
        let token = base.get("t")?.as_dictionary()?;

        Some(plist_get_string!(token, "com.apple.gs.idms.pet", "token"))
    }

    pub fn get_name(&self) -> (String, String) {
        let base = self.spd.as_ref().unwrap();
        (plist_get_string!(base, "fn"), plist_get_string!(base, "ln"))
    }

    pub async fn get_anisette(&self) -> AnisetteData {
        let mut locked = self.anisette.lock().await;

        if locked.needs_refresh() {
            // A failed refresh used to panic, killing the login thread without
            // reporting anything. Keep the headers we already have instead: they
            // may still be inside their window, and if they are not, the request
            // fails with an error the caller can show.
            match locked.refresh().await {
                Ok(refreshed) => *locked = refreshed,
                Err(e) => log::warn!("Could not refresh anisette headers, reusing the current set: {e}"),
            }
        }

        locked.clone()
    }
}

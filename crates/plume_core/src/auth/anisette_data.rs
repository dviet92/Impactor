use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use omnisette::{AnisetteConfiguration, AnisetteError, AnisetteHeaders};

use crate::Error;

#[derive(Debug, Clone)]
pub struct AnisetteData {
    pub base_headers: HashMap<String, String>,
    pub generated_at: SystemTime,
    pub config: AnisetteConfiguration,
}

/// How many times to ask the anisette server before giving up.
const PROVISION_ATTEMPTS: usize = 3;

/// How many times when the server is timing out rather than refusing.
///
/// Creating a device identity from scratch is a websocket session the server
/// times on its own clock, with a round trip to Apple at each step, all through
/// a shared public service. It routinely takes several goes before one gets all
/// the way through, and each failure costs nothing but time. Once the identity
/// exists none of this runs again: later sign-ins on it only fetch headers,
/// which is one quick request.
const PROVISION_TIMEOUT_ATTEMPTS: usize = 6;

/// Waited between attempts, doubling each time.
const PROVISION_BACKOFF: Duration = Duration::from_secs(3);

/// The same, for a timing-out server: start longer, since coming straight back
/// to a server that just ran out of time tends to find it no less busy.
const PROVISION_TIMEOUT_BACKOFF: Duration = Duration::from_secs(5);

/// The doubling stops here, so a long run of failures does not turn into a wait
/// nobody would sit through.
const PROVISION_BACKOFF_CAP: Duration = Duration::from_secs(60);

/// The client-application segment written into `X-MMe-Client-Info`.
///
/// This used to be `com.apple.AuthKit/1 (com.apple.dt.Xcode/3594.4.19)`. Since
/// September 2026 Apple's edge refuses any GrandSlam request whose client info
/// names `com.apple.dt.Xcode`, answering with a generic HTML 503 before the
/// credentials are read -- the outage that took AltStore, Sideloadly and this
/// tool down together. AltStore PR #1790 traced it to exactly this token (user
/// agent, version and hardware changes made no difference) and moved to the
/// `akd` daemon identity, which Apple still accepts. Nothing else about the
/// description changes: it stays `<model> <os;version;build> <this>`.
pub(crate) const GSA_CLIENT_APP: &str = "com.apple.AuthKit/1 (com.apple.akd/1.0)";

impl AnisetteData {
    /// Fetches anisette headers, retrying a failed provisioning.
    ///
    /// An account with no stored provisioning has to negotiate one with the
    /// anisette server, and that negotiation fails often enough to matter --
    /// it is a shared public service and the exchange is several round trips
    /// over a websocket. A refusal is almost always transient, so back off and
    /// ask again rather than failing the sign-in.
    ///
    /// A server that ran out of time is treated more patiently than one that
    /// refused: it is the ordinary way a brand new identity fails to come out
    /// the first time, and waiting it out is the whole fix.
    pub async fn new(config: AnisetteConfiguration) -> Result<Self, Error> {
        let mut backoff = PROVISION_BACKOFF;
        let mut attempt = 1;

        loop {
            let error = match Self::fetch(config.clone()).await {
                Ok(data) => return Ok(data),
                Err(error) => error,
            };

            let timed_out = matches!(error, Error::Anisette(AnisetteError::ProvisioningTimeout));
            let attempts = if timed_out {
                backoff = backoff.max(PROVISION_TIMEOUT_BACKOFF);
                PROVISION_TIMEOUT_ATTEMPTS
            } else {
                PROVISION_ATTEMPTS
            };

            if attempt >= attempts {
                return Err(error);
            }

            log::warn!(
                "Anisette attempt {attempt} of {attempts} failed ({error}); retrying in {}s",
                backoff.as_secs()
            );

            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(PROVISION_BACKOFF_CAP);
            attempt += 1;
        }
    }

    async fn fetch(config: AnisetteConfiguration) -> Result<Self, Error> {
        let mut b = AnisetteHeaders::get_anisette_headers_provider(config.clone())?;
        let base_headers = b.provider.get_authentication_headers().await?;

        Ok(AnisetteData {
            base_headers,
            generated_at: SystemTime::now(),
            config,
        })
    }

    pub fn needs_refresh(&self) -> bool {
        // A clock that went backwards should mean "refresh", not a crash.
        self.age().is_none_or(|age| age.as_secs() > 60)
    }

    pub fn is_valid(&self) -> bool {
        self.age().is_some_and(|age| age.as_secs() < 90)
    }

    fn age(&self) -> Option<std::time::Duration> {
        self.generated_at.elapsed().ok()
    }

    pub async fn refresh(&self) -> Result<Self, crate::Error> {
        Self::new(self.config.clone()).await
    }

    pub fn generate_headers(
        &self,
        cpd: bool,
        client_info: bool,
        app_info: bool,
    ) -> HashMap<String, String> {
        // Stale headers used to abort the process here. Apple rejects them with
        // a normal error, which is recoverable and legible; a panic in the login
        // thread is neither -- it takes the thread down without ever reporting a
        // failure, leaving the sign-in window waiting forever.
        if !self.is_valid() {
            log::warn!(
                "Anisette headers are {}s old and past their window; the request will likely be rejected",
                self.generated_at
                    .elapsed()
                    .map(|elapsed| elapsed.as_secs())
                    .unwrap_or_default()
            );
        }

        let mut headers = self.base_headers.clone();
        let old_client_info = headers.remove("X-Mme-Client-Info");

        if client_info {
            let Some(provided) = old_client_info else {
                return headers;
            };

            // The description is `<model> <os;version;build> <app>`; the third
            // segment names the client app. Swap it for the identity Apple
            // currently accepts (see `GSA_CLIENT_APP`). If the provider ever
            // changes the shape, keep its string as-is rather than panicking
            // mid sign-in.
            let client_info = match provided
                .split('<')
                .nth(3)
                .and_then(|segment| segment.split('>').next())
            {
                Some(app_segment) if !app_segment.is_empty() => {
                    provided.replace(app_segment, GSA_CLIENT_APP)
                }
                _ => provided,
            };

            headers.insert("X-Mme-Client-Info".to_owned(), client_info);
        }

        if app_info {
            headers.insert(
                "X-Apple-App-Info".to_owned(),
                "com.apple.gs.xcode.auth".to_owned(),
            );
            headers.insert("X-Xcode-Version".to_owned(), "11.2 (11B41)".to_owned());
        }

        if cpd {
            headers.insert("bootstrap".to_owned(), "true".to_owned());
            headers.insert("icscrec".to_owned(), "true".to_owned());
            headers.insert("loc".to_owned(), "en_GB".to_owned());
            headers.insert("pbe".to_owned(), "false".to_owned());
            headers.insert("prkgen".to_owned(), "true".to_owned());
            headers.insert("svct".to_owned(), "iCloud".to_owned());
        }

        headers
    }

    pub fn to_plist(&self, cpd: bool, client_info: bool, app_info: bool) -> plist::Dictionary {
        let mut plist = plist::Dictionary::new();
        for (key, value) in self.generate_headers(cpd, client_info, app_info).iter() {
            plist.insert(key.to_owned(), plist::Value::String(value.to_owned()));
        }

        plist
    }

    /// Looks a header up by name, case-insensitively, returning its value as
    /// provisioned.
    ///
    /// This used to lowercase the *value* as well as the name. Sign-in reads
    /// `X-MMe-Client-Info` through here, so every request to Apple carried
    /// `<macbookpro13,2> <macos;13.1;22c65> <com.apple.authkit/1 ...>` -- a
    /// string no real Apple client sends -- instead of the properly cased
    /// device description the anisette provider handed us. Apple's edge has
    /// tightened what it accepts on this endpoint; a malformed client
    /// description is exactly the kind of thing that gets a request refused
    /// before it is even read. Only the name is normalised now.
    pub fn get_header(&self, header: &str) -> Result<String, Error> {
        let wanted = header.to_lowercase();

        self.generate_headers(true, true, true)
            .into_iter()
            .find(|(name, _)| name.to_lowercase() == wanted)
            .map(|(_, value)| value)
            .ok_or(Error::DeveloperSessionRequestFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the anisette provider hands us: still the Xcode identity Apple now
    /// refuses. `generate_headers` must rewrite it, never pass it through.
    const PROVIDED: &str =
        "<MacBookPro13,2> <macOS;13.1;22C65> <com.apple.AuthKit/1 (com.apple.dt.Xcode/3594.4.19)>";

    /// What must actually go on the wire.
    const SENT: &str =
        "<MacBookPro13,2> <macOS;13.1;22C65> <com.apple.AuthKit/1 (com.apple.akd/1.0)>";

    fn data_with_client_info(client_info: &str) -> AnisetteData {
        let mut base_headers = HashMap::new();
        base_headers.insert("X-Mme-Client-Info".to_string(), client_info.to_string());
        base_headers.insert("X-Apple-I-MD-M".to_string(), "AbCdEf0123==".to_string());

        AnisetteData {
            base_headers,
            generated_at: SystemTime::now(),
            config: AnisetteConfiguration::default(),
        }
    }

    #[test]
    fn header_lookup_is_case_insensitive_but_values_keep_their_case() {
        let data = data_with_client_info(PROVIDED);

        // The bug: this used to come back all lowercase.
        assert_eq!(data.get_header("x-mme-client-info").unwrap(), SENT);
        assert_eq!(data.get_header("X-APPLE-I-MD-M").unwrap(), "AbCdEf0123==");
    }

    #[test]
    fn client_info_never_names_xcode() {
        // The token Apple's edge blocks (AltStore PR #1790). Whatever the
        // provider sends, it must not reach the wire.
        for provided in [
            PROVIDED,
            SENT,
            "<iMac20,1> <macOS;15.7;24G222> <anything/1>",
        ] {
            let sent = data_with_client_info(provided)
                .get_header("x-mme-client-info")
                .unwrap();
            assert!(!sent.contains("com.apple.dt.Xcode"), "{sent}");
            assert!(sent.ends_with(&format!("<{GSA_CLIENT_APP}>")), "{sent}");
        }

        // Only the app segment moves; model and OS are the provider's.
        assert_eq!(
            data_with_client_info(PROVIDED)
                .get_header("x-mme-client-info")
                .unwrap(),
            SENT
        );
    }

    #[test]
    fn unknown_header_is_an_error_not_a_panic() {
        assert!(
            data_with_client_info(PROVIDED)
                .get_header("no-such-header")
                .is_err()
        );
    }

    #[test]
    fn odd_client_info_shape_is_passed_through_unchanged() {
        // No third `<...>` segment to rewrite: keep the provider's string rather
        // than panicking mid sign-in.
        let data = data_with_client_info("<OnlyOne>");
        assert_eq!(data.get_header("x-mme-client-info").unwrap(), "<OnlyOne>");
    }
}

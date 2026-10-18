pub mod auth;
pub mod developer;
mod utils;

pub use apple_codesign::{AppleCodesignError, SettingsScope, SigningSettings, UnifiedSigner};

pub use omnisette::AnisetteConfiguration;

pub use utils::{CertificateIdentity, MachO, MachOExt, MobileProvision};

use thiserror::Error as ThisError;
#[derive(Debug, ThisError)]
pub enum Error {
    #[error("Executable not found")]
    BundleExecutableMissing,
    #[error("Entitlements not found")]
    ProvisioningEntitlementsUnknown,
    #[error("Missing certificate PEM data")]
    CertificatePemMissing,
    #[error("Certificate error: {0}")]
    Certificate(String),
    #[error("Developer API error {result_code} (HTTP {http_code:?}): {message} [URL: {url}]")]
    DeveloperApi {
        url: String,
        result_code: i64,
        http_code: Option<u16>,
        message: String,
    },
    #[error("Request to developer session failed")]
    DeveloperSessionRequestFailed,
    #[error("Authentication SRP error {0}: {1}")]
    AuthSrpWithMessage(i64, String),
    #[error("Apple servers busy: {0}")]
    AppleBusy(String),
    /// Apple's edge answered 429: too many requests from this network or
    /// device identity in a short span. Kept apart from [`Error::AppleBusy`]
    /// because the right response differs -- a 5xx is worth one quick retry,
    /// a 429 needs a real wait, and Apple sometimes says how long in
    /// `Retry-After`.
    #[error("Apple rate limit: {detail}")]
    RateLimited {
        detail: String,
        retry_after: Option<std::time::Duration>,
    },
    #[error("Authentication extra step required: {0}")]
    ExtraStep(String),
    #[error("Bad 2FA code")]
    Bad2faCode,
    #[error("Failed to parse")]
    Parse, // TODO: better parsing errors
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// An I/O error that names the file it happened on.
    ///
    /// `Io` above loses the path, and "The system cannot find the file
    /// specified" with no file named is undiagnosable from a screenshot --
    /// it could be the app's executable, a nested framework, a provisioning
    /// profile or the signing key. Every read or write of a named file in
    /// this crate goes through [`Error::io_at`] so the message says which.
    #[error("I/O error on {path}: {source}")]
    IoAt {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("Plist error: {0}")]
    Plist(#[from] plist::Error),
    #[error("Codesign error: {0}")]
    Codesign(#[from] apple_codesign::AppleCodesignError),
    #[error("CodeSignBuilder error: {0}")]
    CodeSignBuilder(#[from] apple_codesign::UniversalMachOError),
    #[error("Certificate PEM error: {0}")]
    Pem(#[from] pem::PemError),
    #[error("X509 certificate error: {0}")]
    X509(#[from] x509_certificate::X509CertificateError),
    #[error("Reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("Anisette error: {0}")]
    Anisette(#[from] omnisette::AnisetteError),
    #[error("Serde JSON error: {0}")]
    SerdeJson(#[from] serde_json::Error),
    #[error("RSA error: {0}")]
    Rsa(#[from] rsa::Error),
    #[error("PKCS1 RSA error: {0}")]
    PKCS1(#[from] rsa::pkcs1::Error),
    #[error("PKCS8 RSA error: {0}")]
    PKCS8(#[from] rsa::pkcs8::Error),
    #[error("RCGen error: {0}")]
    RcGen(#[from] rcgen::Error),
    #[error("AES-GCM error: {0}")]
    AesGcm(#[from] aes_gcm::Error),
    #[error("AES-GCM slice error: {0}")]
    Slice(#[from] std::array::TryFromSliceError),
    #[error("Invalid key length for AES-GCM: {0}")]
    SHA2(#[from] sha2::digest::InvalidLength),
}

impl Error {
    /// Wraps an I/O error with the path it happened on: `.map_err(Error::io_at(&path))`.
    pub fn io_at<P: AsRef<std::path::Path>>(path: P) -> impl FnOnce(std::io::Error) -> Error {
        let path = path.as_ref().to_path_buf();
        move |source| Error::IoAt { path, source }
    }
}

/// Process-wide network settings: the proxy Apple traffic goes through, and
/// how long to wait when Apple answers 429.
///
/// Kept as a global rather than threaded through every constructor because
/// clients are built in many places -- sign-in, token fetch, developer API,
/// anisette provisioning in the omnisette crate, the refresh daemon -- and all
/// of them must agree. The app reads the user's choice from its store at start
/// and on every change and calls [`net::configure`]; everything else just
/// asks.
pub mod net {
    use std::sync::RwLock;
    use std::time::Duration;

    /// Wait after a 429 when the user has not set one: long enough to let
    /// Apple's counter cool, short enough not to feel hung.
    pub const DEFAULT_RATE_LIMIT_WAIT: Duration = Duration::from_secs(30);

    #[derive(Debug, Clone)]
    pub struct Settings {
        /// `http://`, `https://`, `socks5://` or `socks5h://` URL, with
        /// optional `user:pass@`. `None` means direct (or whatever the system
        /// proxy is, via reqwest's `system-proxy` feature).
        pub proxy: Option<String>,
        /// Base wait after a 429; the second wait is twice this.
        pub rate_limit_wait: Duration,
    }

    impl Default for Settings {
        fn default() -> Self {
            Self {
                proxy: None,
                rate_limit_wait: DEFAULT_RATE_LIMIT_WAIT,
            }
        }
    }

    static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

    fn current() -> Settings {
        SETTINGS
            .read()
            .ok()
            .and_then(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Installs the settings for every client built from now on. Also tells
    /// the omnisette crate, whose provisioning requests go to Apple too and
    /// must use the same proxy, or the two halves of a sign-in would come from
    /// two different addresses.
    pub fn configure(settings: Settings) {
        omnisette::set_proxy(settings.proxy.clone());
        if let Ok(mut guard) = SETTINGS.write() {
            *guard = Some(settings);
        }
    }

    pub fn proxy() -> Option<String> {
        current().proxy
    }

    pub fn rate_limit_wait() -> Duration {
        current().rate_limit_wait
    }

    /// Checks that a proxy URL is one reqwest can use, without building a
    /// client: for the settings screen to reject a typo the moment it is typed.
    pub fn validate_proxy(url: &str) -> Result<(), String> {
        let url = url.trim();
        let Some(rest) = ["http://", "https://", "socks5://", "socks5h://", "socks4://"]
            .iter()
            .find_map(|scheme| url.strip_prefix(scheme))
        else {
            return Err(
                "must start with http://, https://, socks5:// or socks5h:// (socks5h resolves \
                 DNS through the proxy)"
                    .to_string(),
            );
        };

        // reqwest accepts `socks5://` with nothing after it; insist on a host.
        let authority = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
        let host = authority
            .split(['/', ':'])
            .next()
            .unwrap_or_default();
        if host.is_empty() {
            return Err("missing host, e.g. socks5://127.0.0.1:1080".to_string());
        }

        reqwest::Proxy::all(url).map(|_| ()).map_err(|e| e.to_string())
    }

    /// One round trip to Apple's sign-in host the way a sign-in would make
    /// it, to show whether the proxy (or the direct route) reaches Apple at
    /// all. Any HTTP status is a success here -- Apple answering is the
    /// point; a 4xx without anisette headers is expected.
    pub async fn test_connection() -> Result<String, String> {
        let started = std::time::Instant::now();
        let client = crate::gsa_client().map_err(|e| e.to_string())?;
        let response = client
            .get("https://gsa.apple.com/grandslam/GsService2/lookup")
            .header("User-Agent", crate::auth::GSA_USER_AGENT)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        Ok(format!(
            "HTTP {} from gsa.apple.com in {} ms, {}",
            response.status(),
            started.elapsed().as_millis(),
            match proxy() {
                Some(_) => "through the proxy",
                None => "direct",
            }
        ))
    }

    /// Routes a client through the configured proxy, if any.
    pub(crate) fn apply_proxy(
        builder: reqwest::ClientBuilder,
    ) -> Result<reqwest::ClientBuilder, crate::Error> {
        match proxy() {
            Some(url) => Ok(builder.proxy(reqwest::Proxy::all(url)?)),
            None => Ok(builder),
        }
    }
}

pub fn client() -> Result<reqwest::Client, Error> {
    const APPLE_ROOT: &[u8] = include_bytes!("./apple_root.der");
    let builder = reqwest::ClientBuilder::new()
        .add_root_certificate(reqwest::Certificate::from_der(APPLE_ROOT)?)
        // uncomment when debugging w/ charles proxy
        // .danger_accept_invalid_certs(true)
        .http1_title_case_headers()
        .connection_verbose(true);
    let client = net::apply_proxy(builder)?.build()?;

    Ok(client)
}

/// The client for the developer API (`developerservices2.apple.com`): the calls
/// signing makes to register a device, ensure an App ID and fetch a
/// provisioning profile.
///
/// Deliberately ignores the configured proxy. The proxy exists for one reason,
/// which is Apple rate limiting *sign-ins* per network address on
/// `gsa.apple.com`; the developer API is authenticated by the account's token
/// and is not part of that counter. Sending it through a proxy anyway only adds
/// a hop to every call in the slowest part of a run -- and a signing run makes
/// several of them per device.
///
/// Connections are pooled here, unlike [`gsa_client`]: this host has no
/// two-requests-per-connection limit, and reusing the connection is most of why
/// the second device is quicker than the first.
pub fn developer_client() -> Result<reqwest::Client, Error> {
    const APPLE_ROOT: &[u8] = include_bytes!("./apple_root.der");
    let client = reqwest::ClientBuilder::new()
        .add_root_certificate(reqwest::Certificate::from_der(APPLE_ROOT)?)
        .http1_title_case_headers()
        .connection_verbose(true)
        .build()?;

    Ok(client)
}

/// The client for GrandSlam (`gsa.apple.com`) sign-in traffic.
///
/// Since around 31 Aug 2026 Apple's edge for `gsa.apple.com` allows only two
/// requests per keep-alive connection: the first two answer with a plist, the
/// third and every one after answer HTTP 503 with an HTML page. Sign-in needs
/// three requests (SRP init, SRP complete, app tokens), and any retry adds
/// more, so a pooled connection is guaranteed to hit the wall and the HTML
/// page then surfaced as `invalid type: string "<html>"`. Apple also pins a
/// keep-alive connection to one backend node, so once that node is failing
/// every request on the connection fails and never recovers.
///
/// Keeping no idle connections means each request opens a fresh connection --
/// the same approach AltStore/AltSign shipped (AltSign PR #52), which took
/// their sign-in success from 2/25 to 25/25. It costs a TLS handshake per
/// request, which is nothing next to sign-in not working at all, and it holds
/// no matter what Apple sets the per-connection limit to.
pub fn gsa_client() -> Result<reqwest::Client, Error> {
    const APPLE_ROOT: &[u8] = include_bytes!("./apple_root.der");
    let builder = reqwest::ClientBuilder::new()
        .add_root_certificate(reqwest::Certificate::from_der(APPLE_ROOT)?)
        .http1_title_case_headers()
        .connection_verbose(true)
        // No idle connections kept: every request gets its own connection.
        .pool_max_idle_per_host(0);
    let client = net::apply_proxy(builder)?.build()?;

    Ok(client)
}

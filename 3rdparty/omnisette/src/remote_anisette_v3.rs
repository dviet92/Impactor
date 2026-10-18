// Implementing the SideStore Anisette v3 protocol

use std::time::{Duration, Instant};
use std::{collections::HashMap, fs, io::Cursor, path::PathBuf};

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose;
use chrono::{DateTime, SubsecRound, Utc};
use futures_util::{SinkExt, stream::StreamExt};
use log::debug;
use plist::{Data, Dictionary};
use reqwest::{Client, ClientBuilder, RequestBuilder};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt::Write;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

use crate::{AnisetteError, anisette_headers_provider::AnisetteHeadersProvider};

fn plist_to_string<T: serde::Serialize>(value: &T) -> Result<String, plist::Error> {
    plist_to_buf(value).map(|val| String::from_utf8(val).unwrap())
}

fn plist_to_buf<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, plist::Error> {
    let mut buf: Vec<u8> = Vec::new();
    let writer = Cursor::new(&mut buf);
    plist::to_writer_xml(writer, &value)?;
    Ok(buf)
}

fn bin_serialize<S>(x: &[u8], s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_bytes(x)
}

fn bin_serialize_opt<S>(x: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    x.clone().map(|i| Data::new(i)).serialize(s)
}

fn bin_deserialize_opt<'de, D>(d: D) -> Result<Option<Vec<u8>>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<Data> = Deserialize::deserialize(d)?;
    Ok(s.map(|i| i.into()))
}

fn bin_deserialize_16<'de, D>(d: D) -> Result<[u8; 16], D::Error>
where
    D: Deserializer<'de>,
{
    let s: Data = Deserialize::deserialize(d)?;
    let s: Vec<u8> = s.into();
    Ok(s.try_into().unwrap())
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        write!(&mut s, "{:02x}", b).unwrap();
    }
    s
}
fn base64_encode(data: &[u8]) -> String {
    general_purpose::STANDARD.encode(data)
}

fn base64_decode(data: &str) -> Vec<u8> {
    general_purpose::STANDARD.decode(data.trim()).unwrap()
}

#[derive(Deserialize)]
struct AnisetteClientInfo {
    client_info: String,
    user_agent: String,
}

#[derive(Serialize, Deserialize)]
pub struct AnisetteState {
    #[serde(
        serialize_with = "bin_serialize",
        deserialize_with = "bin_deserialize_16"
    )]
    keychain_identifier: [u8; 16],
    #[serde(
        serialize_with = "bin_serialize_opt",
        deserialize_with = "bin_deserialize_opt"
    )]
    adi_pb: Option<Vec<u8>>,
}

impl Default for AnisetteState {
    fn default() -> Self {
        AnisetteState {
            keychain_identifier: rand::random::<[u8; 16]>(),
            adi_pb: None,
        }
    }
}

impl AnisetteState {
    pub fn new() -> AnisetteState {
        AnisetteState::default()
    }

    pub fn is_provisioned(&self) -> bool {
        self.adi_pb.is_some()
    }

    fn md_lu(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(&self.keychain_identifier);
        hasher.finalize().into()
    }

    fn device_id(&self) -> String {
        Uuid::from_bytes(self.keychain_identifier).to_string()
    }
}
pub struct AnisetteClient {
    client_info: AnisetteClientInfo,
    url: String,
    /// Built once and reused. Each request still opens its own connection (see
    /// `make_reqwest`); what is saved is rebuilding the TLS stack three times
    /// in the middle of a session the server is timing.
    http: Client,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct ProvisionBodyData {
    header: Dictionary,
    request: Dictionary,
}

#[derive(Debug)]
pub struct AnisetteData {
    machine_id: String,
    one_time_password: String,
    routing_info: String,
    device_description: String,
    local_user_id: String,
    device_unique_identifier: String,
}

impl AnisetteData {
    pub fn get_headers(&self, serial: String) -> HashMap<String, String> {
        let dt: DateTime<Utc> = Utc::now().round_subsecs(0);

        HashMap::from_iter(
            [
                (
                    "X-Apple-I-Client-Time".to_string(),
                    dt.format("%+").to_string().replace("+00:00", "Z"),
                ),
                ("X-Apple-I-SRL-NO".to_string(), serial),
                ("X-Apple-I-TimeZone".to_string(), "UTC".to_string()),
                ("X-Apple-Locale".to_string(), "en_US".to_string()),
                ("X-Apple-I-MD-RINFO".to_string(), self.routing_info.clone()),
                ("X-Apple-I-MD-LU".to_string(), self.local_user_id.clone()),
                (
                    "X-Mme-Device-Id".to_string(),
                    self.device_unique_identifier.clone(),
                ),
                ("X-Apple-I-MD".to_string(), self.one_time_password.clone()),
                ("X-Apple-I-MD-M".to_string(), self.machine_id.clone()),
                (
                    "X-Mme-Client-Info".to_string(),
                    self.device_description.clone(),
                ),
            ]
            .into_iter(),
        )
    }
}

/// How long one request to Apple may take before it is given up on.
///
/// Provisioning runs inside a websocket session the anisette server times out
/// on its own schedule, so a request that hangs does not just cost time -- it
/// costs the whole session, and the next attempt starts from nothing. Failing
/// fast and retrying beats waiting on a request the server has already stopped
/// caring about.
const APPLE_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const APPLE_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

fn make_reqwest() -> Result<Client, AnisetteError> {
    let builder = ClientBuilder::new()
        .http1_title_case_headers()
        .danger_accept_invalid_certs(true) // TODO: pin the apple certificate
        .connect_timeout(APPLE_CONNECT_TIMEOUT)
        .timeout(APPLE_REQUEST_TIMEOUT)
        // Apple's GrandSlam edge answers only two requests per connection and
        // 503s the rest. Provisioning makes three, so each one gets a
        // connection of its own -- which is what building a client per request
        // used to do by accident, and what this keeps doing on purpose now that
        // the client is built once and reused.
        .pool_max_idle_per_host(0);
    Ok(crate::apply_proxy(builder)?.build()?)
}

/// The client application Apple currently accepts in `X-Mme-Client-Info`.
///
/// Since September 2026 Apple's edge refuses any GrandSlam request whose client
/// info names `com.apple.dt.Xcode`, answering with a generic HTML error page
/// (HTTP 503, later 429) before reading the request. The public anisette
/// servers still advertise the Xcode identity from `/v3/client_info`, and this
/// provider used to replay it verbatim to Apple on every provisioning call --
/// `lookup`, `midStartProvisioning`, `midFinishProvisioning` -- so provisioning
/// a fresh identity failed even after the sign-in path had been fixed. The
/// identity is corrected here, at the one place it enters, rather than trusting
/// the server (AltStore PR #1796 does the same on device).
const CLIENT_APP: &str = "com.apple.AuthKit/1 (com.apple.akd/1.0)";

/// Rewrites the client-application segment of a `<model> <os> <app>`
/// description to [`CLIENT_APP`]; an unexpected shape is passed through.
fn accepted_client_info(advertised: &str) -> String {
    match advertised
        .split('<')
        .nth(3)
        .and_then(|segment| segment.split('>').next())
    {
        Some(app) if !app.is_empty() => advertised.replace(app, CLIENT_APP),
        _ => advertised.to_string(),
    }
}

/// Turns Apple's answer to a provisioning request into a plist dictionary,
/// or into an error that says what Apple actually sent.
///
/// These used to be unwrapped: an HTML refusal from Apple's edge was then a
/// plist parse error at best and a panic in the sign-in thread at worst, with
/// nothing saying which request or what status. The status and the start of
/// the body are what tell a rate limit (429) from a blocked identity (503).
fn apple_plist(
    step: &str,
    status: reqwest::StatusCode,
    body: &str,
) -> Result<Dictionary, AnisetteError> {
    let refused = |why: String| {
        let snippet: String = body.trim_start().chars().take(160).collect();
        AnisetteError::ProvisioningFailed(format!(
            "Apple refused {step} (HTTP {status}): {why}; body starts: {snippet:?}"
        ))
    };

    if !status.is_success() {
        return Err(refused("not a success status".to_string()));
    }

    plist::Value::from_reader(Cursor::new(body))
        .map_err(|e| refused(format!("not a plist ({e})")))?
        .into_dictionary()
        .ok_or_else(|| refused("plist is not a dictionary".to_string()))
}

/// `dict[key]` as a string, or an error naming the missing key.
fn plist_str<'d>(
    step: &str,
    dict: &'d Dictionary,
    key: &str,
) -> Result<&'d str, AnisetteError> {
    dict.get(key).and_then(|v| v.as_string()).ok_or_else(|| {
        AnisetteError::ProvisioningFailed(format!("Apple's {step} reply has no `{key}`"))
    })
}

/// `dict[key]` as a dictionary, or an error naming the missing key.
fn plist_dict<'d>(
    step: &str,
    dict: &'d Dictionary,
    key: &str,
) -> Result<&'d Dictionary, AnisetteError> {
    dict.get(key)
        .and_then(|v| v.as_dictionary())
        .ok_or_else(|| {
            AnisetteError::ProvisioningFailed(format!("Apple's {step} reply has no `{key}`"))
        })
}

impl AnisetteClient {
    pub async fn new(url: String) -> Result<AnisetteClient, AnisetteError> {
        let path = format!("{}/v3/client_info", url);
        let http = make_reqwest()?;
        let mut client_info = http
            .get(path)
            .send()
            .await?
            .json::<AnisetteClientInfo>()
            .await?;

        let accepted = accepted_client_info(&client_info.client_info);
        if accepted != client_info.client_info {
            log::info!(
                "anisette server advertises client info {:?}; sending {:?} instead",
                client_info.client_info,
                accepted
            );
            client_info.client_info = accepted;
        }

        Ok(AnisetteClient {
            client_info,
            url,
            http,
        })
    }

    fn build_apple_request(
        &self,
        state: &AnisetteState,
        builder: RequestBuilder,
    ) -> RequestBuilder {
        let dt: DateTime<Utc> = Utc::now().round_subsecs(0);

        builder
            .header("X-Mme-Client-Info", &self.client_info.client_info)
            .header("User-Agent", &self.client_info.user_agent)
            .header("Content-Type", "text/x-xml-plist")
            .header("X-Apple-I-MD-LU", encode_hex(&state.md_lu()))
            .header("X-Mme-Device-Id", state.device_id())
            .header("X-Apple-I-Client-Time", dt.format("%+").to_string())
            .header("X-Apple-I-TimeZone", "UTC")
            .header("X-Apple-Locale", "en_US")
    }

    pub async fn get_headers(&self, state: &AnisetteState) -> Result<AnisetteData, AnisetteError> {
        let path = format!("{}/v3/get_headers", self.url);

        #[derive(Serialize)]
        struct GetHeadersBody {
            identifier: String,
            adi_pb: String,
        }
        let body = GetHeadersBody {
            identifier: base64_encode(&state.keychain_identifier),
            adi_pb: base64_encode(
                state
                    .adi_pb
                    .as_ref()
                    .ok_or(AnisetteError::AnisetteNotProvisioned)?,
            ),
        };

        #[derive(Deserialize)]
        #[serde(tag = "result")]
        enum AnisetteHeaders {
            GetHeadersError {
                message: String,
            },
            Headers {
                #[serde(rename = "X-Apple-I-MD-M")]
                machine_id: String,
                #[serde(rename = "X-Apple-I-MD")]
                one_time_password: String,
                #[serde(rename = "X-Apple-I-MD-RINFO")]
                routing_info: String,
            },
        }

        let headers = self
            .http
            .post(path)
            .json(&body)
            .send()
            .await?
            .json::<AnisetteHeaders>()
            .await?;
        match headers {
            AnisetteHeaders::GetHeadersError { message } => {
                if message.contains("-45061") {
                    Err(AnisetteError::AnisetteNotProvisioned)
                } else {
                    Err(AnisetteError::ProvisioningFailed(format!(
                        "anisette server could not produce headers: {message}"
                    )))
                }
            }
            AnisetteHeaders::Headers {
                machine_id,
                one_time_password,
                routing_info,
            } => Ok(AnisetteData {
                machine_id,
                one_time_password,
                routing_info,
                device_description: self.client_info.client_info.clone(),
                local_user_id: encode_hex(&state.md_lu()),
                device_unique_identifier: state.device_id(),
            }),
        }
    }

    pub async fn provision(&self, state: &mut AnisetteState) -> Result<(), AnisetteError> {
        debug!("Provisioning Anisette");
        let started = Instant::now();
        let resp = self
            .build_apple_request(
                &state,
                self.http
                    .get("https://gsa.apple.com/grandslam/GsService2/lookup"),
            )
            .send()
            .await?;
        log::info!("[anisette] lookup answered in {:?}", started.elapsed());
        let status = resp.status();
        let text = resp.text().await?;

        let protocol_val = apple_plist("lookup", status, &text)?;
        let urls = plist_dict("lookup", &protocol_val, "urls")?;

        let start_provisioning_url = plist_str("lookup", urls, "midStartProvisioning")?;
        let end_provisioning_url = plist_str("lookup", urls, "midFinishProvisioning")?;
        debug!(
            "Got provisioning urls: {} and {}",
            start_provisioning_url, end_provisioning_url
        );

        let provision_ws_url =
            format!("{}/v3/provisioning_session", self.url).replace("https://", "wss://");
        let (mut connection, _) = connect_async(&provision_ws_url).await?;

        #[derive(Deserialize)]
        #[serde(tag = "result")]
        enum ProvisionInput {
            GiveIdentifier,
            GiveStartProvisioningData,
            GiveEndProvisioningData {
                #[allow(dead_code)] // it's not even dead, rust just has problems
                cpim: String,
            },
            ProvisioningSuccess {
                #[allow(dead_code)] // it's not even dead, rust just has problems
                adi_pb: String,
            },
            // The server gave up waiting on its side of the exchange. It is by
            // far the most common way provisioning a fresh identity fails: the
            // server is shared and public, and every step in the middle is a
            // round trip to Apple. Nothing is wrong with the account or the
            // identity, so the caller should simply try again.
            Timeout,
            // The server reports a failed provisioning too. Without this the
            // refusal fails to deserialize and surfaces as "unknown variant",
            // which reads like a client bug and hides what actually went wrong.
            EndProvisioningError {
                #[serde(default, alias = "error", alias = "reason", alias = "detail")]
                message: Option<String>,
            },
            // Anything else the server grows later: report it rather than
            // failing to parse.
            #[serde(other)]
            Unknown,
        }

        loop {
            // Every way the socket can end has to end the loop. `next()` on a
            // finished stream returns `None` immediately and forever, so the
            // old `else { continue }` turned a closed connection into a spin
            // that never came back -- one of the ways creating an identity
            // appeared to hang rather than fail.
            let data = match connection.next().await {
                Some(Ok(data)) => data,
                Some(Err(e)) => {
                    return Err(AnisetteError::ProvisioningFailed(format!(
                        "the provisioning connection failed: {e}"
                    )));
                }
                None => {
                    return Err(AnisetteError::ProvisioningTimeout);
                }
            };

            if data.is_text() {
                let txt = data.to_text().unwrap();
                let msg: ProvisionInput = serde_json::from_str(txt).map_err(|e| {
                    AnisetteError::ProvisioningFailed(format!(
                        "could not read the provisioning server's reply ({e}): {txt}"
                    ))
                })?;
                match msg {
                    ProvisionInput::GiveIdentifier => {
                        #[derive(Serialize)]
                        struct Identifier {
                            identifier: String, // base64
                        }
                        let identifier = Identifier {
                            identifier: base64_encode(&state.keychain_identifier),
                        };
                        connection
                            .send(Message::Text(serde_json::to_string(&identifier)?.into()))
                            .await?;
                    }
                    ProvisionInput::GiveStartProvisioningData => {
                        let started = Instant::now();
                        let body_data = ProvisionBodyData {
                            header: Dictionary::new(),
                            request: Dictionary::new(),
                        };
                        let resp = self
                            .build_apple_request(state, self.http.post(start_provisioning_url))
                            .body(plist_to_string(&body_data)?)
                            .send()
                            .await?;
                        let status = resp.status();
                        let text = resp.text().await?;

                        let step = "midStartProvisioning";
                        let protocol_val = apple_plist(step, status, &text)?;
                        let response = plist_dict(step, &protocol_val, "Response")?;
                        let spim = plist_str(step, response, "spim")?;

                        log::info!(
                            "[anisette] midStartProvisioning answered in {:?}",
                            started.elapsed()
                        );
                        debug!("GiveStartProvisioningData");
                        #[derive(Serialize)]
                        struct Spim {
                            spim: String, // base64
                        }
                        let spim = Spim {
                            spim: spim.to_string(),
                        };
                        connection
                            .send(Message::Text(serde_json::to_string(&spim)?.into()))
                            .await?;
                    }
                    ProvisionInput::GiveEndProvisioningData { cpim } => {
                        let started = Instant::now();
                        let body_data = ProvisionBodyData {
                            header: Dictionary::new(),
                            request: Dictionary::from_iter([("cpim", cpim)].into_iter()),
                        };
                        let resp = self
                            .build_apple_request(state, self.http.post(end_provisioning_url))
                            .body(plist_to_string(&body_data)?)
                            .send()
                            .await?;
                        let status = resp.status();
                        let text = resp.text().await?;

                        let step = "midFinishProvisioning";
                        let protocol_val = apple_plist(step, status, &text)?;
                        let response = plist_dict(step, &protocol_val, "Response")?;

                        log::info!(
                            "[anisette] midFinishProvisioning answered in {:?}",
                            started.elapsed()
                        );
                        debug!("GiveEndProvisioningData");

                        #[derive(Serialize)]
                        struct EndProvisioning<'t> {
                            ptm: &'t str,
                            tk: &'t str,
                        }
                        let end_provisioning = EndProvisioning {
                            ptm: plist_str(step, response, "ptm")?,
                            tk: plist_str(step, response, "tk")?,
                        };
                        connection
                            .send(Message::Text(
                                serde_json::to_string(&end_provisioning)?.into(),
                            ))
                            .await?;
                    }
                    ProvisionInput::ProvisioningSuccess { adi_pb } => {
                        debug!("ProvisioningSuccess");
                        state.adi_pb = Some(base64_decode(&adi_pb));
                        connection.close(None).await?;
                        break;
                    }
                    ProvisionInput::Timeout => {
                        let _ = connection.close(None).await;
                        return Err(AnisetteError::ProvisioningTimeout);
                    }
                    ProvisionInput::EndProvisioningError { message } => {
                        let _ = connection.close(None).await;
                        return Err(AnisetteError::ProvisioningFailed(
                            message.unwrap_or_else(|| {
                                "the anisette server refused to finish provisioning".to_string()
                            }),
                        ));
                    }
                    ProvisionInput::Unknown => {
                        let _ = connection.close(None).await;
                        return Err(AnisetteError::ProvisioningFailed(format!(
                            "unexpected reply from the provisioning server: {txt}"
                        )));
                    }
                }
            } else if data.is_close() {
                break;
            }
        }

        // Reached either by the success arm or by the server hanging up. Only
        // the first leaves an identity behind, and saying so here keeps a
        // half-finished state.plist from being written and then puzzled over.
        if !state.is_provisioned() {
            return Err(AnisetteError::ProvisioningTimeout);
        }

        log::info!("[anisette] identity provisioned in {:?}", started.elapsed());

        Ok(())
    }
}

pub struct RemoteAnisetteProviderV3 {
    client_url: String,
    client: Option<AnisetteClient>,
    pub state: Option<AnisetteState>,
    configuration_path: PathBuf,
    serial: String,
}

impl RemoteAnisetteProviderV3 {
    pub fn new(
        url: String,
        configuration_path: PathBuf,
        serial: String,
    ) -> RemoteAnisetteProviderV3 {
        RemoteAnisetteProviderV3 {
            client_url: url,
            client: None,
            state: None,
            configuration_path,
            serial,
        }
    }
}

#[async_trait]
impl AnisetteHeadersProvider for RemoteAnisetteProviderV3 {
    async fn get_anisette_headers(
        &mut self,
        _skip_provisioning: bool,
    ) -> Result<HashMap<String, String>, AnisetteError> {
        if self.client.is_none() {
            self.client = Some(AnisetteClient::new(self.client_url.clone()).await?);
        }
        let client = self.client.as_ref().unwrap();

        fs::create_dir_all(&self.configuration_path)?;

        let config_path = self.configuration_path.join("state.plist");
        if self.state.is_none() {
            self.state = Some(if let Ok(text) = plist::from_file(&config_path) {
                text
            } else {
                AnisetteState::new()
            });
        }

        let state = self.state.as_mut().unwrap();
        if !state.is_provisioned() {
            client.provision(state).await?;
            plist::to_file_xml(&config_path, state)?;
        }
        let data = match client.get_headers(&state).await {
            Ok(data) => data,
            Err(err) => {
                if matches!(err, AnisetteError::AnisetteNotProvisioned) {
                    state.adi_pb = None;
                    client.provision(state).await?;
                    plist::to_file_xml(config_path, state)?;
                    client.get_headers(&state).await?
                } else {
                    // Used to panic here, which took the sign-in thread down
                    // with no message at all.
                    return Err(err);
                }
            }
        };
        Ok(data.get_headers(self.serial.clone()))
    }
}

#[cfg(test)]
mod client_info_tests {
    use super::accepted_client_info;

    #[test]
    fn xcode_identity_from_the_server_is_replaced() {
        let advertised =
            "<MacBookPro13,2> <macOS;13.1;22C65> <com.apple.AuthKit/1 (com.apple.dt.Xcode/3594.4.19)>";
        let sent = accepted_client_info(advertised);
        assert_eq!(
            sent,
            "<MacBookPro13,2> <macOS;13.1;22C65> <com.apple.AuthKit/1 (com.apple.akd/1.0)>"
        );
        assert!(!sent.contains("com.apple.dt.Xcode"));
    }

    #[test]
    fn an_already_accepted_identity_is_unchanged() {
        let ok = "<Mac17,3> <macOS;27.0;26A428> <com.apple.AuthKit/1 (com.apple.akd/1.0)>";
        assert_eq!(accepted_client_info(ok), ok);
    }

    #[test]
    fn an_odd_shape_is_passed_through() {
        assert_eq!(accepted_client_info("<OnlyOne>"), "<OnlyOne>");
        assert_eq!(accepted_client_info(""), "");
    }
}

#[cfg(test)]
mod tests {
    use crate::anisette_headers_provider::AnisetteHeadersProvider;
    use crate::remote_anisette_v3::RemoteAnisetteProviderV3;
    use crate::{AnisetteError, DEFAULT_ANISETTE_URL_V3};
    use log::info;

    #[tokio::test]
    async fn fetch_anisette_remote_v3() -> Result<(), AnisetteError> {
        crate::tests::init_logger();

        let mut provider = RemoteAnisetteProviderV3::new(
            DEFAULT_ANISETTE_URL_V3.to_string(),
            "anisette_test".into(),
            "0".to_string(),
        );
        info!(
            "Remote headers: {:?}",
            (&mut provider as &mut dyn AnisetteHeadersProvider)
                .get_authentication_headers()
                .await?
        );
        Ok(())
    }
}

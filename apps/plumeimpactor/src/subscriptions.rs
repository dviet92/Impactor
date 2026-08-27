use iced::{Subscription, window};
use idevice::usbmuxd::{UsbmuxdConnection, UsbmuxdListenEvent};
use std::sync::Arc;
use tray_icon::{TrayIconEvent, menu::MenuEvent};

use crate::{
    defaults::get_data_path,
    screen::{Message, general},
};
use plume_utils::{Bundle, Device, PlistInfoTrait};

pub(crate) fn device_listener() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            100,
            |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                use iced::futures::{SinkExt, StreamExt};
                let (tx, mut rx) = iced::futures::channel::mpsc::unbounded::<Message>();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    rt.block_on(async move {
                        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
                        {
                            if let Some(mac_udid) = plume_gestalt::get_udid() {
                                let _ = tx.unbounded_send(Message::DeviceConnected(Device {
                                    name: "This Mac".into(),
                                    os_version: None,
                                    udid: mac_udid,
                                    device_id: u32::MAX,
                                    usbmuxd_device: None,
                                    is_mac: true,
                                }));
                            }
                        }

                        let Ok(mut muxer) = UsbmuxdConnection::default().await else {
                            return;
                        };

                        if let Ok(devices) = muxer.get_devices().await {
                            for dev in devices {
                                let device = Device::new(dev).await;
                                let _ = tx.unbounded_send(Message::DeviceConnected(device));
                            }
                        }

                        let Ok(mut stream) = muxer.listen().await else {
                            return;
                        };

                        while let Some(event) = stream.next().await {
                            let msg = match event {
                                Ok(UsbmuxdListenEvent::Connected(dev)) => {
                                    Message::DeviceConnected(Device::new(dev).await)
                                }
                                Ok(UsbmuxdListenEvent::Disconnected(id)) => {
                                    Message::DeviceDisconnected(id)
                                }
                                Err(_) => continue,
                            };
                            let _ = tx.unbounded_send(msg);
                        }
                    });
                });

                while let Some(message) = rx.next().await {
                    let _ = output.send(message).await;
                }
            },
        )
    })
}

pub(crate) fn tray_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            100,
            |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                use iced::futures::{SinkExt, StreamExt};
                let (tx, mut rx) = iced::futures::channel::mpsc::unbounded::<Message>();

                std::thread::spawn(move || {
                    let menu_channel = MenuEvent::receiver();
                    let tray_channel = TrayIconEvent::receiver();
                    loop {
                        if let Ok(event) = menu_channel.try_recv() {
                            let _ = tx.unbounded_send(Message::TrayMenuClicked(event.id));
                        }

                        if let Ok(event) = tray_channel.try_recv() {
                            match event {
                                TrayIconEvent::DoubleClick {
                                    button: tray_icon::MouseButton::Left,
                                    ..
                                } => {
                                    let _ = tx.unbounded_send(Message::TrayIconClicked);
                                }
                                _ => {}
                            }
                        }

                        #[cfg(target_os = "linux")]
                        {
                            let _ = tx.unbounded_send(Message::GtkTick);
                        }

                        #[cfg(target_os = "macos")]
                        {
                            let _ = tx.unbounded_send(Message::MacOsActivationTick);
                        }

                        std::thread::sleep(std::time::Duration::from_millis(32));
                    }
                });

                while let Some(message) = rx.next().await {
                    let _ = output.send(message).await;
                }
            },
        )
    })
}

pub(crate) fn tray_menu_refresh_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            10,
            |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                use iced::futures::{SinkExt, StreamExt};
                let (tx, mut rx) = iced::futures::channel::mpsc::unbounded::<Message>();

                std::thread::spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(30));
                        let _ = tx.unbounded_send(Message::UpdateTrayMenu);
                    }
                });

                while let Some(message) = rx.next().await {
                    let _ = output.send(message).await;
                }
            },
        )
    })
}

pub(crate) fn certificate_reset_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            10,
            |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                use iced::futures::{SinkExt, StreamExt};
                let (tx, mut rx) = iced::futures::channel::mpsc::unbounded::<Message>();

                std::thread::spawn(move || {
                    while let Some(request) = crate::certificate_reset::wait_for_request() {
                        let _ = tx.unbounded_send(Message::CertificateResetRequested(request));
                    }
                });

                while let Some(message) = rx.next().await {
                    let _ = output.send(message).await;
                }
            },
        )
    })
}

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
pub(crate) fn relaunch_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            10,
            |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                use iced::futures::{SinkExt, StreamExt};
                let (tx, mut rx) = iced::futures::channel::mpsc::unbounded::<Message>();

                if let Err(err) = crate::relaunch::start_listener({
                    let tx = tx.clone();
                    move || {
                        let _ = tx.unbounded_send(Message::RelaunchRequested);
                    }
                }) {
                    log::warn!("Failed to start relaunch listener: {err}");
                }

                while let Some(message) = rx.next().await {
                    let _ = output.send(message).await;
                }
            },
        )
    })
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub(crate) fn relaunch_subscription() -> Subscription<Message> {
    Subscription::none()
}

pub(crate) fn file_hover_subscription() -> Subscription<Message> {
    let window_events = window::events().filter_map(|(_id, event)| match event {
        window::Event::FileHovered(_) => Some(Message::MainScreen(general::Message::FilesHovered)),
        window::Event::FilesHoveredLeft => {
            Some(Message::MainScreen(general::Message::FilesHoveredLeft))
        }
        window::Event::FileDropped(path) => {
            Some(Message::MainScreen(general::Message::FilesDropped(vec![
                path,
            ])))
        }
        _ => None,
    });

    window_events
}

pub(crate) fn installation_progress_listener(
    progress_rx: Option<Arc<std::sync::Mutex<std::sync::mpsc::Receiver<(String, i32)>>>>,
) -> Subscription<(String, i32)> {
    match progress_rx {
        Some(rx) => {
            struct State {
                rx: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<(String, i32)>>>,
            }

            impl std::hash::Hash for State {
                fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                    Arc::as_ptr(&self.rx).hash(state);
                }
            }

            let state = State { rx };
            Subscription::run_with(state, |state| {
                let rx = state.rx.clone();
                iced::stream::channel(
                    100,
                    move |mut output: iced::futures::channel::mpsc::Sender<(String, i32)>| async move {
                        use iced::futures::{SinkExt, StreamExt};

                        let (tx, mut rx_stream) =
                            iced::futures::channel::mpsc::unbounded::<(String, i32)>();

                        let rx_thread = rx.clone();
                        std::thread::spawn(move || {
                            loop {
                                let message = {
                                    if let Ok(guard) = rx_thread.lock() {
                                        guard.try_recv().ok()
                                    } else {
                                        None
                                    }
                                };

                                if let Some((status, progress)) = message {
                                    let _ = tx.unbounded_send((status, progress));
                                }

                                std::thread::sleep(std::time::Duration::from_millis(100));
                            }
                        });

                        while let Some(message) = rx_stream.next().await {
                            let _ = output.send(message).await;
                        }
                    },
                )
            })
        }
        None => Subscription::none(),
    }
}

/// Installs one package onto several devices, one after another.
///
/// Installs are sequential on purpose: they all talk to the same Apple developer
/// account, share one certificate, and can each raise a certificate-reset prompt.
///
/// Each device gets its own account, resolved through the store, so the
/// auto-select setting picks the Apple ID that already provisioned that device
/// and falls back to the ticked one for devices it has never seen.
///
/// A device that fails does not abort the run; the batch finishes and reports
/// everything that went wrong at the end.
/// Recognises "this Apple ID has no room left" from the developer API.
///
/// A free account caps both registered devices and App IDs per week, and either
/// cap means the same thing here: nothing more can be signed with this ID, so
/// move to the next one. The session asks Apple for `en-us`, but the exact
/// wording differs between the two limits and has changed over time, so match on
/// the shape of the message rather than on one fixed string. A false positive
/// only costs a retry on another account.
fn is_account_full_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();

    let capped = message.contains("maximum")
        || message.contains("limit")
        || message.contains("too many")
        || message.contains("exceeded");

    let subject = message.contains("device")
        || message.contains("app id")
        || message.contains("appid")
        || message.contains("register")
        // The free-account app cap words it differently again.
        || message.contains("free development profile");

    capped && subject
}

/// Recognises a stored Apple session that Apple no longer accepts.
///
/// The tokens kept for an account do expire, and once they do every call fails
/// with developer API error 1100 until the account is signed in again. Nothing
/// here can repair that -- the password is not stored -- so the account is out
/// of action for this run.
fn is_session_expired_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();

    message.contains("session has expired")
        || message.contains("please log in")
        || message.contains("error 1100")
}

/// Why an Apple ID could not be used, when that is something another ID can fix.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AccountProblem {
    /// At its weekly device or App ID cap.
    Full,
    /// Signed-in session no longer valid; needs the password again.
    SessionExpired,
}

impl AccountProblem {
    fn detect(message: &str) -> Option<Self> {
        if is_account_full_error(message) {
            Some(Self::Full)
        } else if is_session_expired_error(message) {
            Some(Self::SessionExpired)
        } else {
            None
        }
    }
}

/// Drops the `[URL: ...]` tail the developer API errors carry.
///
/// It is the same eighty characters on every failure and pushes the part that
/// matters off the end of the dialog.
fn tidy_error(message: &str) -> String {
    match message.find(" [URL:") {
        Some(cut) => message[..cut].trim_end().to_string(),
        None => message.to_string(),
    }
}

pub(crate) async fn run_install_batch(
    package: &plume_utils::Package,
    targets: &[Option<Device>],
    options: &plume_utils::SignerOptions,
    store: &mut plume_store::AccountStore,
    tx: &std::sync::mpsc::Sender<(String, i32)>,
) -> Result<(), String> {
    let total = targets.len().max(1);
    // (device name, tidied error) so identical failures can be grouped later.
    let mut failures: Vec<(String, String)> = Vec::new();
    let mut installed = 0usize;

    let auto_select = store.auto_select_account();
    // Accounts that turned out to need a fresh sign-in, reported once at the end
    // rather than repeated per device.
    let mut needs_sign_in: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for (index, target) in targets.iter().enumerate() {
        let udid = target.as_ref().map(|device| device.udid.as_str());
        let name = target
            .as_ref()
            .map(|device| device.name.clone())
            .unwrap_or_default();

        // Only label the steps when there is more than one target, so a plain
        // single install reads exactly as it did before.
        let label = if total > 1 {
            Some(format!("[{}/{}] {}", index + 1, total, name))
        } else {
            None
        };

        let base = index as f32 / total as f32 * 100.0;
        let span = 100.0 / total as f32;

        // Apple IDs this device already tried and that came back full.
        let mut refused: std::collections::HashSet<String> = std::collections::HashSet::new();

        let outcome = loop {
            let account = store
                .auto_account_for_device(udid, chrono::Utc::now(), &refused)
                .cloned();

            if account.is_none() && !refused.is_empty() {
                break Err(format!(
                    "no Apple ID left with room for this device (tried {})",
                    refused.len()
                ));
            }

            // run_installation reports 0..=100 for its own work. Remap that onto
            // this device's slice of the bar and hold it below 100, otherwise the
            // first device finishing would look like the whole batch finishing.
            let (device_tx, device_rx) = std::sync::mpsc::channel::<(String, i32)>();
            let forward_to = tx.clone();
            let step_label = label.clone();
            let forwarder = std::thread::spawn(move || {
                while let Ok((message, progress)) = device_rx.recv() {
                    let progress = progress.clamp(0, 100) as f32;
                    let overall = (base + progress * span / 100.0).min(99.0) as i32;
                    let message = match &step_label {
                        Some(label) => format!("{label} - {message}"),
                        None => message,
                    };
                    let _ = forward_to.send((message, overall));
                }
            });

            let result = run_installation(
                package,
                target.as_ref(),
                options,
                account.as_ref(),
                Some(&mut *store),
                &device_tx,
            )
            .await;

            drop(device_tx);
            let _ = forwarder.join();

            let Err(e) = result else {
                break Ok(());
            };

            // An Apple ID that is full, or whose session has gone stale, is not
            // a failure of this device: another ID may well work, so try one
            // instead of giving up here. With auto-select off the user picked
            // the account deliberately and swapping it behind their back would
            // be wrong.
            let problem = if auto_select && account.is_some() {
                AccountProblem::detect(&e)
            } else {
                None
            };

            let Some(problem) = problem else {
                break Err(e);
            };

            let email = account
                .as_ref()
                .map(|account| account.email().clone())
                .unwrap_or_default();

            log::warn!("Apple ID {email} unusable for {name}: {e}");

            let message = match problem {
                AccountProblem::Full => format!("{email} is full, trying another Apple ID..."),
                AccountProblem::SessionExpired => {
                    needs_sign_in.insert(email.clone());
                    format!("{email} needs signing in again, trying another Apple ID...")
                }
            };

            let _ = tx.send((
                match &label {
                    Some(label) => format!("{label} - {message}"),
                    None => message,
                },
                base as i32,
            ));

            refused.insert(email);
        };

        match outcome {
            Ok(()) => installed += 1,
            Err(e) => {
                log::error!("Install failed for {name}: {e}");
                failures.push((name.clone(), tidy_error(&e)));
            }
        }
    }

    if failures.is_empty() {
        return Ok(());
    }

    // Devices commonly share a name and fail for the same reason, so listing
    // every one repeats the same sentence until the useful part is off screen.
    // Group by message and count instead.
    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    for (name, message) in failures {
        match grouped.iter_mut().find(|(seen, _)| *seen == message) {
            Some((_, names)) => names.push(name),
            None => grouped.push((message, vec![name])),
        }
    }

    let mut lines = vec![format!("{installed} of {total} succeeded.")];

    for (message, names) in grouped {
        if names.len() == 1 {
            lines.push(format!("{}: {message}", names[0]));
        } else {
            lines.push(format!("{} devices: {message}", names.len()));
        }
    }

    if !needs_sign_in.is_empty() {
        lines.push(format!(
            "Sign in again in Settings: {}",
            needs_sign_in.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    Err(lines.join("\n"))
}

pub(crate) async fn run_installation(
    package: &plume_utils::Package,
    device: Option<&Device>,
    options: &plume_utils::SignerOptions,
    account: Option<&plume_store::GsaAccount>,
    mut store: Option<&mut plume_store::AccountStore>,
    tx: &std::sync::mpsc::Sender<(String, i32)>,
) -> Result<(), String> {
    use plume_core::{AnisetteConfiguration, CertificateIdentity, developer::DeveloperSession};
    use plume_utils::{Signer, SignerInstallMode, SignerMode};

    let package_file: Bundle;
    let mut options = options.clone();
    let send = |msg: String, progress: i32| {
        let _ = tx.send((msg, progress));
    };

    send("Preparing package...".to_string(), 10);

    match options.mode {
        SignerMode::Pem => {
            let Some(account) = account else {
                return Err("GSA account is required for PEM signing".to_string());
            };

            send("Ensuring account is valid...".to_string(), 20);

            let session = DeveloperSession::new(
                account.adsid().clone(),
                account.xcode_gs_token().clone(),
                AnisetteConfiguration::default()
                    .set_configuration_path(crate::defaults::get_anisette_path(account.email())),
            )
            .await
            .map_err(|e| e.to_string())?;

            let teams_response = session.qh_list_teams().await.map_err(|e| e.to_string())?;

            if teams_response.teams.is_empty() {
                return Err("No teams available for this account".to_string());
            }

            let team_id = account.team_id();

            if !team_id.is_empty() && !teams_response.teams.iter().any(|t| &t.team_id == team_id) {
                return Err(format!(
                    "Stored team ID '{}' not found in available teams. Please update your team selection in Settings.",
                    team_id
                ));
            }

            let team_id = if team_id.is_empty() {
                &teams_response.teams[0].team_id
            } else {
                team_id
            };

            let mut on_certificate_reset = || {
                send(crate::certificate_reset::WARNING.to_string(), 20);
                crate::certificate_reset::confirm()
            };
            let identity = CertificateIdentity::new_with_session(
                &session,
                crate::defaults::get_data_path(),
                None,
                team_id,
                false,
                Some(&mut on_certificate_reset),
            )
            .await
            .map_err(|e| e.to_string())?;

            send("Ensuring device is registered...".to_string(), 30);

            if let Some(dev) = &device {
                session
                    .qh_ensure_device(team_id, &dev.name, &dev.udid)
                    .await
                    .map_err(|e| e.to_string())?;
            }

            send("Extracting package...".to_string(), 50);

            let mut signer = Signer::new(Some(identity), options.clone());

            let bundle = package.get_package_bundle().map_err(|e| e.to_string())?;

            send("Signing package...".to_string(), 70);

            signer
                .modify_bundle(&bundle, &Some(team_id.clone()))
                .await
                .map_err(|e| e.to_string())?;
            signer
                .register_bundle(&bundle, &session, team_id, false)
                .await
                .map_err(|e| e.to_string())?;
            signer
                .sign_bundle(&bundle)
                .await
                .map_err(|e| e.to_string())?;

            options = signer.options.clone();
            package_file = bundle;
        }
        SignerMode::Adhoc => {
            send("Extracting package...".to_string(), 50);

            let mut signer = Signer::new(None, options.clone());

            let bundle = package.get_package_bundle().map_err(|e| e.to_string())?;

            send("Signing package...".to_string(), 70);

            signer
                .modify_bundle(&bundle, &None)
                .await
                .map_err(|e| e.to_string())?;
            signer
                .sign_bundle(&bundle)
                .await
                .map_err(|e| e.to_string())?;

            options = signer.options.clone();
            package_file = bundle;
        }
        _ => {
            send("Extracting package...".to_string(), 50);

            let bundle = package.get_package_bundle().map_err(|e| e.to_string())?;

            package_file = bundle;
        }
    }

    match options.install_mode {
        SignerInstallMode::Install => {
            if let Some(dev) = &device {
                if !dev.is_mac {
                    send("Sending to device...".to_string(), 70);

                    let tx_clone = tx.clone();
                    dev.install_app(&package_file.bundle_dir(), move |progress: i32| {
                        let tx = tx_clone.clone();
                        // Some libraries expect this future to be processed.
                        // We ensure it sends and resolves immediately.
                        Box::pin(async move {
                            let _ = tx.send(("Installing...".to_string(), 70 + (progress / 5)));
                        })
                    })
                    .await
                    .map_err(|e| format!("Install error: {}", e))?;

                    if options.app.supports_pairing_file() {
                        if let (Some(custom_identifier), Some(pairing_file_bundle_path)) = (
                            options.custom_identifier.as_ref(),
                            options.app.pairing_file_path(),
                        ) {
                            let _ = dev
                                .install_pairing_record(
                                    custom_identifier,
                                    &pairing_file_bundle_path,
                                )
                                .await;
                        }
                    }

                    // The install went through, so remember which Apple ID provisioned
                    // this device. That is what the auto-select option reads next time.
                    // Ad-hoc installs have no account to remember.
                    if options.mode == SignerMode::Pem {
                        if let (Some(account), Some(store)) = (account, store.as_mut()) {
                            if let Err(e) =
                                store.record_device_account_sync(&dev.udid, account.email())
                            {
                                log::warn!(
                                    "Failed to remember account for device {}: {e}",
                                    dev.udid
                                );
                            }
                        }
                    }
                } else {
                    send("Installing...".to_string(), 90);

                    plume_utils::install_app_mac(&package_file.bundle_dir())
                        .await
                        .map_err(|e| e.to_string())?;
                }
            } else {
                return Err("No device connected for installation".to_string());
            }
        }
        SignerInstallMode::Export => {
            send("Exporting...".to_string(), 90);

            let archive_path = package
                .get_archive_based_on_path(&package_file.bundle_dir())
                .map_err(|e| e.to_string())?;

            let file = rfd::AsyncFileDialog::new()
                .set_title("Save Package As")
                .set_file_name(
                    archive_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("package.ipa"),
                )
                .save_file()
                .await;

            if let Some(save_path) = file {
                tokio::fs::copy(&archive_path, &save_path.path())
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    if options.refresh && options.mode == SignerMode::Pem {
        send("Saving for refresh...".to_string(), 75);
        let path = get_data_path().join("refresh_store");
        tokio::fs::create_dir_all(&path)
            .await
            .map_err(|e| e.to_string())?;

        let original_name = package_file
            .bundle_dir()
            .file_name()
            .unwrap()
            .to_string_lossy();
        let uuid = uuid::Uuid::new_v4();
        let dest_name = if let Some(dot_pos) = original_name.rfind('.') {
            let (name, ext) = original_name.split_at(dot_pos);
            format!("{}-{}{}", name, uuid, ext)
        } else {
            format!("{}-{}", original_name, uuid)
        };
        let dest_path = path.join(dest_name);

        plume_utils::copy_dir_recursively(&package_file.bundle_dir(), &dest_path)
            .await
            .map_err(|e| e.to_string())?;

        if let (Some(dev), Some(account), Some(store)) = (&device, &account, store.as_mut()) {
            let embedded_prov_path = dest_path.join("embedded.mobileprovision");

            let provision_path = if embedded_prov_path.exists() {
                Some(embedded_prov_path)
            } else {
                None
            };

            if let Some(prov_path) = provision_path {
                use plume_core::MobileProvision;

                if let Ok(provision) = MobileProvision::load_with_path(&prov_path) {
                    let expiration_date = provision.expiration_date().clone();
                    let scheduled_refresh = expiration_date
                        .to_xml_format()
                        .parse::<chrono::DateTime<chrono::Utc>>()
                        .unwrap_or_else(|_| chrono::Utc::now() + chrono::Duration::days(4));
                    let scheduled_refresh = scheduled_refresh - chrono::Duration::days(3);

                    let refresh_app = plume_store::RefreshApp {
                        name: package_file.get_name(),
                        bundle_id: package_file.get_bundle_identifier(),
                        path: dest_path.clone(),
                        scheduled_refresh,
                    };

                    let mut refresh_device = store
                        .get_refresh_device(&dev.udid)
                        .cloned()
                        .unwrap_or_else(|| plume_store::RefreshDevice {
                            udid: dev.udid.clone(),
                            name: dev.name.clone(),
                            account: account.email().clone(),
                            apps: Vec::new(),
                            is_mac: dev.is_mac,
                        });

                    if let Some(existing_app) = refresh_device
                        .apps
                        .iter_mut()
                        .find(|a| a.bundle_id == refresh_app.bundle_id)
                    {
                        *existing_app = refresh_app;
                    } else {
                        refresh_device.apps.push(refresh_app);
                    }

                    store
                        .add_or_update_refresh_device_sync(refresh_device)
                        .map_err(|e| e.to_string())?;
                }
            }
        }
    }

    send("Finished!".to_string(), 100);

    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn export_certificate(account: plume_store::GsaAccount) -> Result<(), String> {
    use plume_core::{AnisetteConfiguration, CertificateIdentity, developer::DeveloperSession};

    let session = DeveloperSession::new(
        account.adsid().clone(),
        account.xcode_gs_token().clone(),
        AnisetteConfiguration::default()
            .set_configuration_path(crate::defaults::get_anisette_path(account.email())),
    )
    .await
    .map_err(|e| e.to_string())?;

    let teams_response = session.qh_list_teams().await.map_err(|e| e.to_string())?;

    if teams_response.teams.is_empty() {
        return Err("No teams available for this account".to_string());
    }

    let team_id = account.team_id();

    if !team_id.is_empty() && !teams_response.teams.iter().any(|t| &t.team_id == team_id) {
        return Err(format!(
            "Stored team ID '{}' not found in available teams. Please update your team selection in Settings.",
            team_id
        ));
    }

    let team_id = if team_id.is_empty() {
        &teams_response.teams[0].team_id
    } else {
        team_id
    };

    let mut on_certificate_reset = crate::certificate_reset::confirm;
    let identity = CertificateIdentity::new_with_session(
        &session,
        crate::defaults::get_data_path(),
        None,
        team_id,
        true,
        Some(&mut on_certificate_reset),
    )
    .await
    .map_err(|e| e.to_string())?;

    let Some(p12_data) = identity.p12_data else {
        return Err("Missing p12 data".to_string());
    };

    let archive_path =
        crate::defaults::get_data_path().join(format!("{}_certificate.p12", team_id));
    tokio::fs::write(&archive_path, p12_data)
        .await
        .map_err(|e| e.to_string())?;

    let file = rfd::AsyncFileDialog::new()
        .set_title("Save Certificate As")
        .set_file_name(
            archive_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("certificate.p12"),
        )
        .save_file()
        .await;

    if let Some(save_path) = file {
        tokio::fs::copy(&archive_path, &save_path.path())
            .await
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

pub(crate) async fn fetch_teams(
    account: &plume_store::GsaAccount,
) -> Result<Vec<crate::screen::settings::Team>, String> {
    use plume_core::{AnisetteConfiguration, developer::DeveloperSession};

    let session = DeveloperSession::new(
        account.adsid().clone(),
        account.xcode_gs_token().clone(),
        AnisetteConfiguration::default()
            .set_configuration_path(crate::defaults::get_anisette_path(account.email())),
    )
    .await
    .map_err(|e| e.to_string())?;

    let teams_response = session.qh_list_teams().await.map_err(|e| e.to_string())?;

    Ok(teams_response
        .teams
        .into_iter()
        .map(|t| crate::screen::settings::Team {
            name: t.name,
            id: t.team_id,
        })
        .collect())
}

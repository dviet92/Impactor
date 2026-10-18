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

/// Live progress for a run over several devices, one lane per worker.
///
/// A lane is one Apple ID's worth of work: sign once, then send to that ID's
/// devices one after another. Lanes run side by side, so the progress screen
/// draws a bar per lane with the devices it is carrying listed underneath --
/// the number of bars is the number of things actually happening at once.
///
/// Kept in a process-wide slot rather than threaded through the progress
/// channel: only one install runs at a time (the progress screen is modal), and
/// the channel's `(String, i32)` shape is shared with the single-device path,
/// which has no use for lanes. Every change here is followed by a message on
/// that channel, and that message is what makes the screen redraw.
pub(crate) mod lanes {
    use std::sync::Mutex;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum State {
        Waiting,
        Working,
        Done,
        Failed,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct Lane {
        pub title: String,
        pub percent: i32,
        pub status: String,
        pub state: State,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct DeviceLine {
        pub udid: String,
        pub name: String,
        /// The lane carrying it; `None` while it waits to be given one.
        pub lane: Option<usize>,
        pub percent: i32,
        pub state: State,
        pub note: String,
    }

    #[derive(Debug, Clone, Default)]
    pub(crate) struct Board {
        pub lanes: Vec<Lane>,
        pub devices: Vec<DeviceLine>,
    }

    static BOARD: Mutex<Board> = Mutex::new(Board {
        lanes: Vec::new(),
        devices: Vec::new(),
    });

    fn with<R>(f: impl FnOnce(&mut Board) -> R) -> R {
        // A panic while the lock was held leaves nothing half-written that
        // matters for drawing, so carry on with what is there.
        let mut board = BOARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut board)
    }

    fn finished(state: State) -> bool {
        matches!(state, State::Done | State::Failed)
    }

    /// Empties the board. Called when a new install starts, so a single-device
    /// install never shows the rows of the multi-install before it.
    pub(crate) fn clear() {
        with(|board| *board = Board::default());
    }

    /// Every device of the run, waiting and not yet on any lane.
    pub(crate) fn start<'a>(devices: impl IntoIterator<Item = (&'a str, &'a str)>) {
        with(|board| {
            *board = Board {
                lanes: Vec::new(),
                devices: devices
                    .into_iter()
                    .map(|(udid, name)| DeviceLine {
                        udid: udid.to_string(),
                        name: name.to_string(),
                        lane: None,
                        percent: 0,
                        state: State::Waiting,
                        note: String::new(),
                    })
                    .collect(),
            };
        });
    }

    /// Opens a lane for `udids` and returns its number. It shows as waiting
    /// until its first progress report.
    pub(crate) fn open(title: impl Into<String>, udids: &[String], status: impl Into<String>) -> usize {
        with(|board| {
            let lane = board.lanes.len();
            board.lanes.push(Lane {
                title: title.into(),
                percent: 0,
                status: status.into(),
                state: State::Waiting,
            });
            for device in board
                .devices
                .iter_mut()
                .filter(|device| udids.contains(&device.udid))
            {
                device.lane = Some(lane);
                device.state = State::Waiting;
                device.percent = 0;
                device.note = "Queued on this Apple ID".to_string();
            }
            lane
        })
    }

    /// Moves a lane on. A finished lane is left alone, so a late report cannot
    /// turn a failure back into "working".
    pub(crate) fn progress(lane: usize, percent: i32, status: impl Into<String>) {
        let status = status.into();
        with(|board| {
            if let Some(lane) = board.lanes.get_mut(lane) {
                if finished(lane.state) {
                    return;
                }
                lane.state = State::Working;
                lane.percent = percent.clamp(0, 100);
                lane.status = status;
            }
        });
    }

    /// Ends a lane, with the line it should keep showing.
    pub(crate) fn close(lane: usize, ok: bool, status: impl Into<String>) {
        let status = status.into();
        with(|board| {
            if let Some(lane) = board.lanes.get_mut(lane) {
                lane.state = if ok { State::Done } else { State::Failed };
                if ok {
                    lane.percent = 100;
                }
                lane.status = status;
            }
        });
    }

    /// A device on its lane has started, or moved on.
    pub(crate) fn device_working(udid: &str, percent: i32, note: impl Into<String>) {
        let note = note.into();
        with(|board| {
            if let Some(device) = board.devices.iter_mut().find(|device| device.udid == udid) {
                if finished(device.state) {
                    return;
                }
                device.state = State::Working;
                device.percent = percent.clamp(0, 100);
                device.note = note;
            }
        });
    }

    /// Takes a device off its lane and back into the queue -- its Apple ID
    /// was refused, and it will be tried again with another.
    pub(crate) fn device_requeued(udid: &str, note: impl Into<String>) {
        let note = note.into();
        with(|board| {
            if let Some(device) = board.devices.iter_mut().find(|device| device.udid == udid) {
                device.lane = None;
                device.state = State::Waiting;
                device.percent = 0;
                device.note = note;
            }
        });
    }

    /// Marks a device finished, either way.
    pub(crate) fn device_finish(udid: &str, result: Result<(), &str>) {
        with(|board| {
            if let Some(device) = board.devices.iter_mut().find(|device| device.udid == udid) {
                device.percent = 100;
                match result {
                    Ok(()) => {
                        device.state = State::Done;
                        device.note = "Installed".to_string();
                    }
                    Err(reason) => {
                        device.state = State::Failed;
                        device.note = reason.to_string();
                    }
                }
            }
        });
    }

    /// How far the run as a whole has got, from every device: finished ones
    /// count in full, the rest by how far their own install has got. Held
    /// below 100 so the run only reads as finished when it says so.
    pub(crate) fn overall() -> i32 {
        with(|board| {
            if board.devices.is_empty() {
                return 0;
            }
            let sum: i32 = board.devices.iter().map(|device| device.percent).sum();
            (sum / board.devices.len() as i32).min(99)
        })
    }

    /// A copy of the board, for drawing.
    pub(crate) fn snapshot() -> Board {
        with(|board| board.clone())
    }
}

/// Marks every device in `devices` as failed for the same reason.
///
/// Both halves matter: the run summary needs the name, and the store needs it
/// too, so the Multi Install list can still say why the row is red after the
/// app has been closed and reopened.
fn record_failures(
    store: &mut plume_store::AccountStore,
    failures: &mut Vec<(String, String)>,
    devices: &[Device],
    reason: &str,
) {
    for device in devices {
        failures.push((device.name.clone(), reason.to_string()));
        lanes::device_finish(&device.udid, Err(reason));
        if let Err(e) = store.record_install_result_sync(&device.udid, false, Some(reason)) {
            log::warn!("Failed to save install status for {}: {e}", device.udid);
        }
    }
}

/// Runs the package against every target.
///
/// Two ways through. When every target is a real iOS device and the run is an
/// install, the devices are grouped by the Apple ID they will be signed with,
/// each group is signed **once**, and the result is sent to that group's
/// devices at the same time. The provisioning profile Apple hands back names
/// the team's devices rather than one of them, so a single signature covers
/// every device registered before it was made -- signing again per device
/// produces a different file that says exactly the same thing, and pays for
/// the codesign and the Apple round trips again to do it.
///
/// Anything else -- an export, a Mac target, a single device -- takes the
/// device-after-device path, which is simpler and has nothing to gain here.
pub(crate) async fn run_install_batch(
    package: &plume_utils::Package,
    targets: &[Option<Device>],
    options: &plume_utils::SignerOptions,
    store: &mut plume_store::AccountStore,
    tx: &std::sync::mpsc::Sender<(String, i32)>,
) -> Result<(), String> {
    let groupable = targets.len() > 1
        && options.install_mode == plume_utils::SignerInstallMode::Install
        && targets
            .iter()
            .all(|target| target.as_ref().is_some_and(|device| !device.is_mac));

    if !groupable {
        return run_install_batch_serial(package, targets, options, store, tx).await;
    }

    let devices: Vec<Device> = targets.iter().flatten().cloned().collect();
    run_install_batch_grouped(package, &devices, options, store, tx).await
}

async fn run_install_batch_serial(
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

    // A lane per device only when there is more than one; a plain single
    // install keeps the one bar it always had.
    if total > 1 {
        lanes::start(
            targets
                .iter()
                .flatten()
                .map(|device| (device.udid.as_str(), device.name.as_str())),
        );
    }

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

        // Device after device, so each is its own lane while it runs.
        let lane = match udid {
            Some(udid) if total > 1 => Some(lanes::open(
                name.clone(),
                &[udid.to_string()],
                "Starting...",
            )),
            _ => None,
        };

        let outcome = loop {
            let account = store
                .auto_account_for_device(udid, chrono::Utc::now(), &refused)
                .cloned();

            // Auto-select returns nothing once every Apple ID is carrying its
            // cap of devices. Signing would fail at Apple with a far less
            // obvious error, so stop here and name the real reason. Ad-hoc
            // signing needs no account at all, hence the mode check.
            if account.is_none() && options.mode == plume_utils::SignerMode::Pem {
                break Err(if !refused.is_empty() {
                    format!(
                        "no Apple ID left with room for this device (tried {})",
                        refused.len()
                    )
                } else if auto_select {
                    format!(
                        "every Apple ID already carries {} devices in the last {} days; \
                         remove a device from one, or add another Apple ID",
                        plume_store::AUTO_ACCOUNT_MAX_DEVICES,
                        plume_store::AUTO_ACCOUNT_WINDOW_DAYS
                    )
                } else {
                    "no Apple ID selected in Settings".to_string()
                });
            }

            // run_installation reports 0..=100 for its own work. Remap that onto
            // this device's slice of the bar and hold it below 100, otherwise the
            // first device finishing would look like the whole batch finishing.
            let (device_tx, device_rx) = std::sync::mpsc::channel::<(String, i32)>();
            let forward_to = tx.clone();
            let step_label = label.clone();
            let lane_udid = udid.map(str::to_string);
            let forwarder = std::thread::spawn(move || {
                while let Ok((message, progress)) = device_rx.recv() {
                    if let (Some(lane), Some(udid)) = (lane, &lane_udid) {
                        lanes::progress(lane, progress, message.clone());
                        lanes::device_working(udid, progress, message.clone());
                    }
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

            if let Some(lane) = lane {
                lanes::progress(lane, 0, message.clone());
            }

            let _ = tx.send((
                match &label {
                    Some(label) => format!("{label} - {message}"),
                    None => message,
                },
                base as i32,
            ));

            refused.insert(email);
        };

        let reason = match &outcome {
            Ok(()) => {
                installed += 1;
                None
            }
            Err(e) => {
                log::error!("Install failed for {name}: {e}");
                let reason = tidy_error(e);
                failures.push((name.clone(), reason.clone()));
                Some(reason)
            }
        };

        // Save how this device went, right now -- not at the end of the batch --
        // so the picker can colour it, and so a crash later in the run does not
        // lose the devices already finished. The reason rides along so a red
        // row can say what went wrong instead of only that it did. The export
        // path (a `None` target) has no device to mark, so it is skipped.
        if let Some(device) = target.as_ref() {
            lanes::device_finish(&device.udid, reason.as_deref().map_or(Ok(()), Err));
            if let Some(lane) = lane {
                match &reason {
                    None => lanes::close(lane, true, "Installed"),
                    Some(reason) => lanes::close(lane, false, reason.clone()),
                }
            }
            if let Err(e) = store.record_install_result_sync(
                &device.udid,
                reason.is_none(),
                reason.as_deref(),
            ) {
                log::warn!("Failed to save install status for {}: {e}", device.udid);
            }
            // The lane changed; this is what makes the screen show it.
            if total > 1 {
                let _ = tx.send((
                    match &reason {
                        None => format!("{name} installed"),
                        Some(_) => format!("{name} failed"),
                    },
                    ((index + 1) as f32 / total as f32 * 100.0).min(99.0) as i32,
                ));
            }
        }
    }

    summarise(installed, total, failures, needs_sign_in)
}

/// How many device names a shared failure is allowed to spell out before the
/// line switches to a count.
const NAMED_FAILURES: usize = 3;

/// Turns what a run managed and what it did not into the single message the
/// progress screen shows.
///
/// Devices commonly share a name and fail for the same reason, so listing every
/// one repeats the same sentence until the useful part is off the screen.
/// Identical messages are gathered onto one line instead, which names the
/// devices while there are few enough to fit and counts them after that. The
/// full per-device reason stays in the store either way, under the red rows in
/// Multi Install.
fn summarise(
    installed: usize,
    total: usize,
    failures: Vec<(String, String)>,
    needs_sign_in: std::collections::BTreeSet<String>,
) -> Result<(), String> {
    if failures.is_empty() {
        return Ok(());
    }

    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    for (name, message) in failures {
        match grouped.iter_mut().find(|(seen, _)| *seen == message) {
            Some((_, names)) => names.push(name),
            None => grouped.push((message, vec![name])),
        }
    }

    let mut lines = vec![format!("{installed} of {total} succeeded.")];

    for (message, names) in grouped {
        lines.push(format!("{}: {message}", name_list(&names)));
    }

    if !needs_sign_in.is_empty() {
        lines.push(format!(
            "Sign in again in Settings: {}",
            needs_sign_in.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    lines.push(String::from(
        "Each failed device keeps its reason under its name in Multi Install.",
    ));

    Err(lines.join("\n"))
}

/// The device names in front of a shared failure message.
///
/// Up to [`NAMED_FAILURES`] are spelled out; beyond that the extras become a
/// count, so a run over twenty devices does not bury the reason itself.
fn name_list(names: &[String]) -> String {
    if names.len() <= NAMED_FAILURES {
        return names.join(", ");
    }

    format!(
        "{}, and {} more",
        names[..NAMED_FAILURES].join(", "),
        names.len() - NAMED_FAILURES
    )
}

/// Where one lane reports to: its row on the board, and the shared channel
/// whose messages make the screen redraw.
struct GroupProgress {
    tx: std::sync::mpsc::Sender<(String, i32)>,
    lane: usize,
    /// Shown in front of this lane's messages on the single status line, which
    /// every lane writes to.
    title: String,
    /// The lane's devices. Steps shared by all of them -- registering, the one
    /// signature -- move each of them along together.
    udids: Vec<String>,
}

impl GroupProgress {
    /// A step of the whole lane, `fraction` of the way through its signing.
    fn send(&self, message: impl Into<String>, fraction: f32) {
        let message = message.into();
        let percent = (fraction.clamp(0.0, 1.0) * 100.0) as i32;
        lanes::progress(self.lane, percent, message.clone());
        for udid in &self.udids {
            lanes::device_working(udid, percent, message.clone());
        }
        self.redraw(message);
    }

    /// Posts `message` on the shared status line, with the run's overall
    /// percentage worked out from the board.
    fn redraw(&self, message: impl Into<String>) {
        let _ = self
            .tx
            .send((format!("{} - {}", self.title, message.into()), lanes::overall()));
    }
}

/// What became of one lane.
enum GroupOutcome {
    /// Signed; one result per device, in the order they were given.
    Signed(Vec<(String, Result<(), String>)>),
    /// Never got as far as a signature.
    NotSigned(String),
}

/// Installs onto devices by Apple ID.
///
/// Each Apple ID is one lane: its devices are registered, the package is
/// signed once for all of them, and the result goes to them one after another.
/// Lanes for different Apple IDs run side by side, up to the "parallel
/// installs" setting. Devices sharing an Apple ID are deliberately kept on one
/// lane: they share one signature, and splitting them would mean signing again
/// for the same account at the same time -- the same Apple round trips twice,
/// racing over the same certificate and profile.
async fn run_install_batch_grouped(
    package: &plume_utils::Package,
    devices: &[Device],
    options: &plume_utils::SignerOptions,
    store: &mut plume_store::AccountStore,
    tx: &std::sync::mpsc::Sender<(String, i32)>,
) -> Result<(), String> {
    use futures::stream::StreamExt;

    let total = devices.len();
    // Clamped rather than trusted: zero would run nothing, and a very large
    // number only moves the bottleneck onto Apple and the USB tree.
    let parallel = store.network().install_parallel.clamp(1, 16) as usize;
    let auto_select = store.auto_select_account();
    let needs_account = options.mode == plume_utils::SignerMode::Pem;

    let mut pending: Vec<Device> = devices.to_vec();
    let mut refused: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut needs_sign_in: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut failures: Vec<(String, String)> = Vec::new();
    let mut installed = 0usize;

    lanes::start(
        devices
            .iter()
            .map(|device| (device.udid.as_str(), device.name.as_str())),
    );

    // The lanes each need the store -- to count devices against their Apple
    // ID and to save them for refresh -- while running at the same time, so
    // they share it behind a lock. Every write goes straight to disk, and the
    // copy is handed back once the run is over.
    let shared = Arc::new(tokio::sync::Mutex::new(store.clone()));

    // Each round plans every pending device onto an Apple ID and runs those
    // lanes. A lane whose Apple ID turns out full or signed out sends its
    // devices back for the next round, with that ID ruled out; there are
    // finitely many IDs to rule out, so this ends.
    while !pending.is_empty() {
        let (groups, unplanned) = {
            let guard = shared.lock().await;
            match plan_groups(&guard, &pending, &refused, auto_select, needs_account) {
                Ok(plan) => plan,
                Err(reason) => {
                    drop(guard);
                    let mut guard = shared.lock().await;
                    record_failures(&mut guard, &mut failures, &pending, &reason);
                    pending.clear();
                    break;
                }
            }
        };
        pending.clear();

        if !unplanned.is_empty() {
            let reason = format!(
                "no Apple ID has room for this device (each carries {} devices in {} days)",
                plume_store::AUTO_ACCOUNT_MAX_DEVICES,
                plume_store::AUTO_ACCOUNT_WINDOW_DAYS
            );
            let mut guard = shared.lock().await;
            record_failures(&mut guard, &mut failures, &unplanned, &reason);
        }

        // Every lane of the round goes on the board before any starts, so the
        // screen shows the whole plan -- including the lanes still waiting for
        // a free slot.
        let mut work = Vec::with_capacity(groups.len());
        for (email, group) in groups {
            let udids: Vec<String> = group.iter().map(|device| device.udid.clone()).collect();
            let title = match &email {
                Some(email) => format!("{email} ({} device(s))", group.len()),
                None => format!("{} device(s)", group.len()),
            };
            let lane = lanes::open(title.clone(), &udids, "Waiting for a free slot");

            let account = match &email {
                Some(email) => shared.lock().await.get_account(email).cloned(),
                None => None,
            };

            work.push((
                email,
                group,
                account,
                GroupProgress {
                    tx: tx.clone(),
                    lane,
                    title,
                    udids,
                },
            ));
        }

        let running = work.into_iter().map(|(email, group, account, progress)| {
            let package = package.clone();
            let options = options.clone();
            let shared = shared.clone();

            async move {
                let lane = progress.lane;

                if needs_account && account.is_none() {
                    let outcome = GroupOutcome::NotSigned(format!(
                        "{} is no longer in the account list",
                        email.clone().unwrap_or_default()
                    ));
                    return (email, group, outcome, lane);
                }

                // Its own thread and runtime, so one lane's signing -- heavy on
                // the CPU -- runs on another core instead of stalling the
                // others. (The signer's futures cannot move between threads,
                // so a plain task would not do.)
                let group_for_task = group.clone();
                let task = tokio::task::spawn_blocking(move || {
                    let outcome = match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => runtime.block_on(async {
                            match install_group(
                                &package,
                                &group_for_task,
                                &options,
                                account.as_ref(),
                                &shared,
                                &progress,
                            )
                            .await
                            {
                                Ok(results) => GroupOutcome::Signed(results),
                                Err(e) => GroupOutcome::NotSigned(e),
                            }
                        }),
                        Err(e) => GroupOutcome::NotSigned(format!(
                            "could not start an install worker: {e}"
                        )),
                    };
                    package.remove_bundle_slot(lane);
                    outcome
                });

                let outcome = task.await.unwrap_or_else(|e| {
                    GroupOutcome::NotSigned(format!("the install worker stopped: {e}"))
                });
                (email, group, outcome, lane)
            }
        });

        let finished: Vec<_> = futures::stream::iter(running)
            .buffer_unordered(parallel)
            .collect()
            .await;

        let mut requeue: Vec<Device> = Vec::new();

        for (email, group, outcome, lane) in finished {
            match outcome {
                GroupOutcome::Signed(results) => {
                    let mut ok_count = 0usize;
                    let mut guard = shared.lock().await;
                    for (udid, result) in results {
                        let name = group
                            .iter()
                            .find(|device| device.udid == udid)
                            .map(|device| device.name.clone())
                            .unwrap_or_default();

                        let reason = match result {
                            Ok(()) => {
                                installed += 1;
                                ok_count += 1;
                                None
                            }
                            Err(e) => {
                                log::error!("Install failed for {name}: {e}");
                                let reason = tidy_error(&e);
                                failures.push((name, reason.clone()));
                                Some(reason)
                            }
                        };

                        if let Err(e) = guard.record_install_result_sync(
                            &udid,
                            reason.is_none(),
                            reason.as_deref(),
                        ) {
                            log::warn!("Failed to save install status for {udid}: {e}");
                        }
                    }
                    lanes::close(
                        lane,
                        ok_count == group.len(),
                        format!("{ok_count}/{} installed", group.len()),
                    );
                }
                // Nothing was signed, so nothing was spent: an Apple ID that is
                // full or stale is worth swapping for another, exactly as the
                // device-at-a-time path does.
                GroupOutcome::NotSigned(e) => {
                    let problem = if auto_select && email.is_some() {
                        AccountProblem::detect(&e)
                    } else {
                        None
                    };

                    let Some(problem) = problem else {
                        let message = tidy_error(&e);
                        log::error!("Signing failed for {} device(s): {e}", group.len());
                        let mut guard = shared.lock().await;
                        record_failures(&mut guard, &mut failures, &group, &message);
                        lanes::close(lane, false, message);
                        continue;
                    };

                    let email = email.unwrap_or_default();
                    log::warn!(
                        "Apple ID {email} unusable for {} device(s): {e}",
                        group.len()
                    );

                    let message = match problem {
                        AccountProblem::Full => {
                            format!("{email} is full, moving its devices to another Apple ID")
                        }
                        AccountProblem::SessionExpired => {
                            needs_sign_in.insert(email.clone());
                            format!(
                                "{email} needs signing in again, moving its devices to another Apple ID"
                            )
                        }
                    };
                    lanes::close(lane, false, message.clone());
                    for device in &group {
                        lanes::device_requeued(&device.udid, message.clone());
                    }
                    let _ = tx.send((message, lanes::overall()));
                    refused.insert(email);
                    requeue.extend(group);
                }
            }
        }

        // Back in the order the user ticked them, so the re-plan fills Apple
        // IDs the same way a fresh run would.
        requeue.sort_by_key(|device| {
            devices
                .iter()
                .position(|original| original.udid == device.udid)
                .unwrap_or(usize::MAX)
        });
        pending = requeue;
    }

    *store = shared.lock().await.clone();
    summarise(installed, total, failures, needs_sign_in)
}

/// The turn-taking lock for one identity group's anisette state; see its use
/// in [`install_group`]. `None` is the ungrouped identity.
fn identity_group_lock(group: Option<u32>) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::Mutex<
        Option<std::collections::HashMap<Option<u32>, Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::Mutex::new(None);

    let mut locks = LOCKS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    locks
        .get_or_insert_with(std::collections::HashMap::new)
        .entry(group)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Every pending device placed on an Apple ID: the groups, in the order the
/// run reaches them, and the devices no Apple ID had room for.
fn plan_groups(
    store: &plume_store::AccountStore,
    pending: &[Device],
    refused: &std::collections::HashSet<String>,
    auto_select: bool,
    needs_account: bool,
) -> Result<(Vec<(Option<String>, Vec<Device>)>, Vec<Device>), String> {
    // Ad-hoc signing answers to no Apple ID, so one signature covers the lot.
    if !needs_account {
        return Ok((vec![(None, pending.to_vec())], Vec::new()));
    }

    if !auto_select {
        let Some(email) = store
            .selected_account()
            .map(|account| account.email().clone())
        else {
            return Err("no Apple ID selected in Settings".to_string());
        };

        if refused.contains(&email) {
            return Err(format!("{email} could not be used"));
        }

        return Ok((vec![(Some(email), pending.to_vec())], Vec::new()));
    }

    let udids: Vec<String> = pending.iter().map(|device| device.udid.clone()).collect();
    let (planned, unassigned) = store.plan_device_accounts(&udids, refused);

    if planned.is_empty() {
        return Err(format!(
            "every Apple ID already carries {} devices in the last {} days; \
             remove a device from one, or add another Apple ID",
            plume_store::AUTO_ACCOUNT_MAX_DEVICES,
            plume_store::AUTO_ACCOUNT_WINDOW_DAYS
        ));
    }

    let pick = |udids: &[String]| -> Vec<Device> {
        pending
            .iter()
            .filter(|device| udids.contains(&device.udid))
            .cloned()
            .collect()
    };

    let groups = planned
        .into_iter()
        .map(|(email, udids)| (Some(email), pick(&udids)))
        .collect();

    Ok((groups, pick(&unassigned)))
}

/// Signs the package once for `devices`, then sends it to each in turn.
///
/// `Err` means the group never reached a signed bundle -- an Apple ID problem,
/// or signing itself failing -- which is the caller's cue to try another Apple
/// ID. `Ok` carries one result per device, in the order they were given: the
/// bundle exists, and each transfer succeeded or failed on its own.
async fn install_group(
    package: &plume_utils::Package,
    devices: &[Device],
    options: &plume_utils::SignerOptions,
    account: Option<&plume_store::GsaAccount>,
    store: &tokio::sync::Mutex<plume_store::AccountStore>,
    progress: &GroupProgress,
) -> Result<Vec<(String, Result<(), String>)>, String> {
    use plume_core::{AnisetteConfiguration, CertificateIdentity, developer::DeveloperSession};
    use plume_utils::{Signer, SignerMode};

    let mut options = options.clone();
    let package_file: Bundle;
    // A private extraction per lane: lanes run at the same time, and the
    // shared one is wiped on every use.
    let slot = progress.lane;

    match options.mode {
        SignerMode::Pem => {
            let Some(account) = account else {
                return Err("GSA account is required for PEM signing".to_string());
            };

            progress.send("Ensuring account is valid...", 0.02);

            // Apple IDs grouped into one device identity share its anisette
            // state on disk. Opening a session reads it -- and rewrites it if
            // Apple asks for provisioning again -- so two lanes of the same
            // identity group take turns here; a half-written state read by the
            // other would otherwise look like no state and mint a new identity.
            let identity_lock = identity_group_lock(account.identity_group());
            let identity_turn = identity_lock.lock().await;

            let session = DeveloperSession::new(
                account.adsid().clone(),
                account.xcode_gs_token().clone(),
                AnisetteConfiguration::default().set_configuration_path(
                    crate::defaults::get_anisette_path(account.identity_group()),
                ),
            )
            .await
            .map_err(|e| e.to_string())?;

            let teams_response = session.qh_list_teams().await.map_err(|e| e.to_string())?;
            drop(identity_turn);

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
                progress.send(crate::certificate_reset::WARNING.to_string(), 0.02);
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

            // Every device first, then one signature. The profile fetched below
            // lists the devices the team had at that moment, so a device
            // registered afterwards would simply not be on it -- which is the
            // whole reason this loop comes before the signing.
            for (index, device) in devices.iter().enumerate() {
                progress.send(
                    format!(
                        "Registering {} ({}/{})...",
                        device.name,
                        index + 1,
                        devices.len()
                    ),
                    0.04 + 0.10 * (index as f32 / devices.len() as f32),
                );

                session
                    .qh_ensure_device(team_id, &device.name, &device.udid)
                    .await
                    .map_err(|e| e.to_string())?;
            }

            progress.send("Extracting package...", 0.16);

            let mut signer = Signer::new(Some(identity), options.clone());
            let bundle = package
                .get_package_bundle_in_slot(slot)
                .map_err(|e| e.to_string())?;

            progress.send(
                format!("Signing once for {} device(s)...", devices.len()),
                0.20,
            );

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

            // The account's device slot is spent for every device in the group
            // the moment the signature exists, whether or not each transfer
            // below goes through -- that is what Apple has seen.
            {
                let mut store = store.lock().await;
                for device in devices {
                    if let Err(e) = store.record_device_account_sync(&device.udid, account.email())
                    {
                        log::warn!("Failed to remember account for device {}: {e}", device.udid);
                    }
                }
            }

            options = signer.options.clone();
            package_file = bundle;
        }
        SignerMode::Adhoc => {
            progress.send("Extracting package...", 0.16);

            let mut signer = Signer::new(None, options.clone());
            let bundle = package
                .get_package_bundle_in_slot(slot)
                .map_err(|e| e.to_string())?;

            progress.send("Signing package...", 0.20);

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
            progress.send("Extracting package...", 0.16);
            package_file = package
                .get_package_bundle_in_slot(slot)
                .map_err(|e| e.to_string())?;
        }
    }

    progress.send("Signed", SIGNING_SHARE);

    let mut results = Vec::with_capacity(devices.len());

    // One device at a time: this lane is one Apple ID, and its devices take
    // turns with the one signed bundle.
    for (index, device) in devices.iter().enumerate() {
        let result = transfer_to_device(&package_file, device, index, devices.len(), progress).await;

        if result.is_ok() {
            if options.app.supports_pairing_file() {
                if let (Some(custom_identifier), Some(pairing_file_bundle_path)) = (
                    options.custom_identifier.as_ref(),
                    options.app.pairing_file_path(),
                ) {
                    let _ = device
                        .install_pairing_record(custom_identifier, &pairing_file_bundle_path)
                        .await;
                }
            }

            if options.refresh && options.mode == SignerMode::Pem {
                if let Some(account) = account {
                    lanes::device_working(&device.udid, 99, "Saving for refresh...");
                    progress.redraw(format!("Saving {} for refresh...", device.name));

                    let mut store = store.lock().await;
                    if let Err(e) =
                        remember_for_refresh(&package_file, device, account, &mut store).await
                    {
                        log::warn!("Failed to save {} for refresh: {e}", device.name);
                    }
                }
            }
        }

        let reason = result.as_ref().err().map(|e| tidy_error(e));
        lanes::device_finish(&device.udid, reason.as_deref().map_or(Ok(()), Err));
        progress.redraw(match &reason {
            None => format!("{} installed", device.name),
            Some(_) => format!("{} failed", device.name),
        });

        results.push((device.udid.clone(), result));
    }

    Ok(results)
}

/// Sends the signed bundle to one device of a lane, moving both the device's
/// row and the lane's bar as it goes.
async fn transfer_to_device(
    package_file: &Bundle,
    device: &Device,
    index: usize,
    count: usize,
    progress: &GroupProgress,
) -> Result<(), String> {
    let bundle_dir = package_file.bundle_dir().clone();
    let count = count.max(1) as f32;
    let label = format!("Installing on {} ({}/{})", device.name, index + 1, count as usize);

    // The device's own row covers its whole journey -- the shared signing is
    // the first stretch, its transfer the rest -- and the lane's bar covers the
    // signing plus each of its devices in turn.
    let device_percent =
        |percent: i32| (SIGNING_SHARE * 100.0 + (1.0 - SIGNING_SHARE) * percent as f32) as i32;
    let lane_percent = move |percent: i32| {
        let done = index as f32 + percent.clamp(0, 100) as f32 / 100.0;
        (SIGNING_SHARE * 100.0 + (1.0 - SIGNING_SHARE) * 100.0 * done / count) as i32
    };

    lanes::progress(progress.lane, lane_percent(0), format!("{label}..."));
    lanes::device_working(&device.udid, device_percent(0), "Sending...");
    progress.redraw(format!("{label}..."));

    let tx = progress.tx.clone();
    let lane = progress.lane;
    let title = progress.title.clone();
    let udid = device.udid.clone();

    device
        .install_app(&bundle_dir, move |percent: i32| {
            lanes::progress(lane, lane_percent(percent), format!("{label}... {percent}%"));
            lanes::device_working(&udid, device_percent(percent), format!("Installing... {percent}%"));

            let tx = tx.clone();
            let message = format!("{title} - {label}... {percent}%");
            // The caller expects this future to be driven, so it sends and
            // resolves straight away.
            Box::pin(async move {
                let _ = tx.send((message, lanes::overall()));
            })
        })
        .await
        .map_err(|e| format!("Install error: {e}"))
}

/// How much of a group's slice of the bar belongs to getting to a signed
/// bundle; the rest belongs to the transfers.
const SIGNING_SHARE: f32 = 0.3;

/// Copies the signed app aside and schedules it to be signed again before its
/// profile expires.
///
/// One copy per device: each keeps the profile it was installed with, which is
/// what the refresh daemon works from later.
async fn remember_for_refresh(
    package_file: &Bundle,
    device: &Device,
    account: &plume_store::GsaAccount,
    store: &mut plume_store::AccountStore,
) -> Result<(), String> {
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

    // No profile means nothing to schedule against; the copy is harmless.
    let embedded_prov_path = dest_path.join("embedded.mobileprovision");
    if !embedded_prov_path.exists() {
        return Ok(());
    }

    use plume_core::MobileProvision;

    let Ok(provision) = MobileProvision::load_with_path(&embedded_prov_path) else {
        return Ok(());
    };

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
        .get_refresh_device(&device.udid)
        .cloned()
        .unwrap_or_else(|| plume_store::RefreshDevice {
            udid: device.udid.clone(),
            name: device.name.clone(),
            account: account.email().clone(),
            apps: Vec::new(),
            is_mac: device.is_mac,
        });

    if let Some(existing_app) = refresh_device
        .apps
        .iter_mut()
        .find(|app| app.bundle_id == refresh_app.bundle_id)
    {
        *existing_app = refresh_app;
    } else {
        refresh_device.apps.push(refresh_app);
    }

    store
        .add_or_update_refresh_device_sync(refresh_device)
        .map_err(|e| e.to_string())
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
                AnisetteConfiguration::default().set_configuration_path(
                    crate::defaults::get_anisette_path(account.identity_group()),
                ),
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

            // The device is registered and the app signed for it, so the
            // account's certificate and device slot are spent now, whether or
            // not the transfer to the device below goes through. Recording here
            // rather than after the install means a failed install still counts
            // for auto-select -- which matches what Apple has seen. Only for an
            // install to a real device: an export signs nothing onto a device.
            if options.install_mode == SignerInstallMode::Install {
                if let (Some(dev), Some(store)) = (&device, store.as_mut()) {
                    if !dev.is_mac {
                        if let Err(e) = store.record_device_account_sync(&dev.udid, account.email())
                        {
                            log::warn!("Failed to remember account for device {}: {e}", dev.udid);
                        }
                    }
                }
            }

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

                    // Which Apple ID provisioned this device was already
                    // recorded at signing time, above: the slot is spent the
                    // moment the app is signed, even if the install then fails.
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

        // Only worth keeping when there is a device to refresh onto and an
        // account to sign it again with; an export has neither, and used to
        // leave a copy behind anyway.
        if let (Some(dev), Some(account), Some(store)) = (device, account, store.as_deref_mut()) {
            remember_for_refresh(&package_file, dev, account, store).await?;
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
        AnisetteConfiguration::default().set_configuration_path(
            crate::defaults::get_anisette_path(account.identity_group()),
        ),
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
        AnisetteConfiguration::default().set_configuration_path(
            crate::defaults::get_anisette_path(account.identity_group()),
        ),
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

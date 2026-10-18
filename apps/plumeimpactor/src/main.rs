#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

rust_i18n::i18n!("../../locales", fallback = "en");

use crate::refresh::spawn_refresh_daemon;

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
use single_instance::SingleInstance;

mod appearance;
mod certificate_reset;
mod defaults;
mod macos_app;
mod refresh;
mod relaunch;
mod screen;
mod startup;
mod subscriptions;
mod tray;

pub const APP_NAME: &str = "Impactor";
pub const APP_NAME_VERSIONED: &str = concat!("Impactor", " - Version ", env!("CARGO_PKG_VERSION"));

fn main() -> iced::Result {
    init_logging();
    install_crash_logger();

    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    // English only. The language picker was removed: one language means one
    // set of strings to keep right, and the technical messages (Apple's
    // errors, rate limits, proxies) read better untranslated anyway.
    rust_i18n::set_locale("en");

    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    let _single_instance = match SingleInstance::new(&crate::relaunch::single_instance_key()) {
        Ok(instance) => {
            if !instance.is_single() {
                if let Err(err) = crate::relaunch::notify_running_instance() {
                    log::warn!("Failed to signal existing instance: {err}");
                }
                log::info!("Another instance is already running; exiting.");
                return Ok(());
            }

            // We hold the only lock, so every staging directory still sitting in
            // temp belongs to a run that did not get to clean up after itself --
            // a crash, or an install that was still in flight at exit. Each one
            // holds a full copy of an IPA plus its extraction, so left alone
            // they fill the disk, and the next package to be staged is the one
            // that pays for it.
            plume_utils::Package::clean_orphan_stages();

            Some(instance)
        }
        Err(err) => {
            log::warn!("Failed to acquire single-instance lock: {err}");
            None
        }
    };

    // For tray on linux.
    #[cfg(target_os = "linux")]
    {
        gtk::init().expect("GTK init failed");
    }

    // For notifications on macOS.
    #[cfg(target_os = "macos")]
    {
        notify_rust::get_bundle_identifier_or_default("Impactor");
        notify_rust::set_application("dev.khcrysalis.PlumeImpactor").ok();
    }

    // Before anything spawns a thread: this sets an environment variable, which
    // is only sound while the process is still single threaded.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    choose_renderer();

    let (_daemon_handle, connected_devices) = spawn_refresh_daemon();
    screen::set_refresh_daemon_devices(connected_devices);

    run_daemon()
}

/// Sends log output to `impactor.log` in the data folder as well as stderr.
///
/// A release build has no console, so anything logged only to stderr is lost
/// -- which left a failing sign-in with nothing to go on but the one line the
/// window shows. `RUST_LOG` still overrides the filter; without it our own
/// crates log at info and everything else at warn, so the file stays readable.
/// The file is appended to across runs; it is safe to delete at any time.
fn init_logging() {
    use std::io::Write;

    /// Writes to the log file and mirrors to stderr, so a debug build keeps
    /// its console output and a release build gets the file.
    struct Tee(std::fs::File);

    impl Write for Tee {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let _ = std::io::stderr().write_all(buf);
            self.0.write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            let _ = std::io::stderr().flush();
            self.0.flush()
        }
    }

    let env = env_logger::Env::default().default_filter_or(
        "warn,plumeimpactor=info,plume_core=info,plume_store=info,plume_utils=info,omnisette=info",
    );
    let mut builder = env_logger::Builder::from_env(env);
    builder.format_timestamp_secs();

    let path = defaults::get_data_path().join("impactor.log");
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => {
            builder.target(env_logger::Target::Pipe(Box::new(Tee(file))));
        }
        Err(e) => eprintln!(
            "Could not open {} for logging ({e}); logging to stderr only",
            path.display()
        ),
    }

    builder.init();

    log::info!("==== Impactor {} starting ====", env!("CARGO_PKG_VERSION"));
}

/// Writes every panic to `crash.log` in the data directory before it unwinds.
///
/// A release build detaches from the console, so a panic leaves nothing on
/// screen -- the window simply disappears. This records what happened (thread,
/// message, and the source location) to a file the user can send back, so a
/// silent crash stops being a dead end. The default hook still runs, so debug
/// builds keep printing to the console as before.
fn install_crash_logger() {
    let default_hook = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        // UTC rather than local time: `Utc::now()` is already used across the
        // app, so it is guaranteed available regardless of which chrono features
        // the dependency tree happens to enable.
        let when = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
        let thread = std::thread::current();
        let thread = thread.name().unwrap_or("unnamed").to_string();

        // `info.location()` is present even when symbols are stripped, so the
        // file and line that panicked are always recorded.
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown location".to_string());

        let message = info.to_string();

        let entry = format!(
            "\n===== {when} =====\nthread: {thread}\nat: {location}\n{message}\n\
             backtrace:\n{}\n",
            std::backtrace::Backtrace::force_capture()
        );

        let path = defaults::get_data_path().join("crash.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            use std::io::Write;
            let _ = file.write_all(entry.as_bytes());
        }

        log::error!("Panic ({thread}) at {location}: {message}");

        default_hook(info);
    }));
}

/// Falls back to the CPU renderer when the GPU cannot be trusted with drawing.
///
/// wgpu is the right default and stays the default wherever there is a real
/// GPU. But a headless Windows Server, a virtual machine, or a remote-desktop
/// session offers either no adapter at all or a software one, and iced's wgpu
/// backend on those fails in a way that reads as the app hanging rather than as
/// a graphics error: the window stops presenting and Windows leaves the last
/// frame on screen, stretched and washed out, until the app is restarted.
/// Opening the sign-in window is what usually tips it over, because that is the
/// moment a second surface has to be created.
///
/// This ran on Linux only, which is why those machines were fine and Windows
/// Server was not.
///
/// RE: https://github.com/claration/Impactor/issues/103,
/// https://github.com/claration/Impactor/issues/90
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn choose_renderer() {
    // An explicit choice always wins, so there is a way out when this probe
    // reads a machine wrong in either direction:
    //     ICED_BACKEND=tiny-skia   force the CPU renderer
    //     ICED_BACKEND=wgpu        force the GPU renderer
    if std::env::var_os("ICED_BACKEND").is_some() {
        log::info!("ICED_BACKEND is already set; leaving the renderer alone.");
        return;
    }

    let instance = wgpu::Instance::default();

    let adapter =
        iced::futures::executor::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }));

    let unusable = match adapter {
        Err(e) => Some(format!("no adapter found: {e}")),
        Ok(adapter) => {
            let info = adapter.get_info();

            if info.device_type == wgpu::DeviceType::Cpu {
                // A software adapter draws on the CPU regardless, only with a
                // driver in the way that the CPU renderer does not have.
                Some(format!("{} is a software adapter", info.name))
            } else if !adapter.features().contains(wgpu::Features::SHADER_F16) {
                Some(format!("{} has no FP16 support", info.name))
            } else {
                log::info!("Using the wgpu renderer on {}.", info.name);
                None
            }
        }
    };

    if let Some(reason) = unusable {
        log::warn!("Falling back to the tiny-skia renderer: {reason}");

        // Sound only because nothing else has been spawned yet; see the call site.
        unsafe {
            std::env::set_var("ICED_BACKEND", "tiny-skia");
        }
    }
}

fn run_daemon() -> iced::Result {
    iced::daemon(
        screen::Impactor::new,
        screen::Impactor::update,
        screen::Impactor::view,
    )
    .subscription(screen::Impactor::subscription)
    .title(APP_NAME_VERSIONED)
    .theme(appearance::PlumeTheme::default().to_iced_theme())
    .settings(defaults::default_settings())
    .run()
}

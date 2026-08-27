use std::{
    env, fs,
    path::{Path, PathBuf},
};

use iced::window;

use crate::appearance;

pub(crate) fn default_settings() -> iced::Settings {
    iced::Settings {
        default_font: appearance::p_font(),
        default_text_size: appearance::THEME_FONT_SIZE.into(),
        fonts: appearance::load_fonts(),
        ..Default::default()
    }
}

pub(crate) fn default_window_settings() -> window::Settings {
    #[cfg(target_os = "macos")]
    let platform_specific = window::settings::PlatformSpecific {
        titlebar_transparent: true,
        title_hidden: true,
        fullsize_content_view: true,
        ..Default::default()
    };

    #[cfg(target_os = "linux")]
    let platform_specific = window::settings::PlatformSpecific {
        application_id: String::from("dev.khcrysalis.PlumeImpactor"),
        ..Default::default()
    };

    #[cfg(target_os = "windows")]
    let platform_specific = window::settings::PlatformSpecific::default();

    window::Settings {
        // Sized to fit the installer screen (the tallest) without scrolling.
        size: iced::Size::new(575.0, 475.0),
        position: window::Position::Centered,
        exit_on_close_request: false,
        resizable: false,
        icon: Some(load_window_icon()),
        platform_specific: platform_specific,
        ..Default::default()
    }
}

fn load_window_icon() -> window::Icon {
    let bytes = include_bytes!(
        "../../../package/linux/icons/hicolor/64x64/apps/dev.khcrysalis.PlumeImpactor.png"
    );
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .expect("Failed to load icon bytes")
        .to_rgba8();
    let (width, height) = image.dimensions();
    window::icon::from_rgba(image.into_raw(), width, height).unwrap()
}

/// Anisette state directory for one Apple ID.
///
/// Every sign-in carries a device fingerprint built from the `state.plist` in
/// this directory: `X-Mme-Device-Id` is a UUID over the random
/// `keychain_identifier` stored there, `X-Apple-I-MD-LU` is a hash of the same
/// bytes, and `X-Apple-I-MD-M` comes from the ADI provisioning kept alongside
/// it. One shared directory made every account look like the same machine to
/// Apple, which is what ties them together. One directory per account gives
/// each its own fingerprint.
pub fn get_anisette_path(email: &str) -> PathBuf {
    // Lower cased so the same account reached through different capitalisation
    // does not end up with two identities.
    let email = email.trim().to_ascii_lowercase();

    // A readable stem, plus a hash because the stem alone can collide once the
    // characters a path cannot hold have been replaced.
    let stem: String = email
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '.' | '-' => c,
            _ => '_',
        })
        .take(40)
        .collect();

    let dir = get_data_path()
        .join("anisette")
        .join(format!("{stem}-{:016x}", fnv1a(&email)));

    fs::create_dir_all(&dir).ok();

    dir
}

/// FNV-1a, written out rather than pulled in.
///
/// This value becomes part of a directory name that has to resolve to the same
/// account on every build and platform, for as long as the install lives.
/// `DefaultHasher` guarantees neither.
fn fnv1a(value: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;

    for byte in value.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }

    hash
}

pub fn get_data_path() -> PathBuf {
    let base = if cfg!(windows) {
        env::var("APPDATA").unwrap()
    } else {
        env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
            env::var("HOME").unwrap() + "/.config"
        })
    };

    let dir = Path::new(&base).join("PlumeImpactor");

    fs::create_dir_all(&dir).ok();

    dir
}

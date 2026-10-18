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

/// The anisette state directory for an identity group, or the machine-wide one.
///
/// Every sign-in carries a device fingerprint built from the `state.plist` in
/// this directory: `X-Mme-Device-Id` is a UUID over the random
/// `keychain_identifier` stored in it, `X-Apple-I-MD-LU` is a hash of the same
/// bytes, and `X-Apple-I-MD-M` comes from the ADI provisioning kept alongside.
///
/// `None` is the machine identity, kept directly in the data directory as it
/// always has been, so accounts signed in before grouping existed keep
/// working. `Some(n)` is `anisette/group-n/`: a separate fingerprint shared by
/// up to the "max Apple IDs per identity" setting's worth of Apple IDs. A directory with no
/// `state.plist` yet is provisioned by omnisette on first use, which is the
/// slow, failure-prone step -- hence groups rather than one identity per
/// account. Which one an account uses is recorded on the account itself
/// ([`plume_store::GsaAccount::identity_group`]) and must not change after
/// sign-in, because Apple binds the issued tokens to the fingerprint.
pub fn get_anisette_path(group: Option<u32>) -> PathBuf {
    match group {
        None => get_data_path(),
        Some(group) => {
            let dir = get_data_path()
                .join("anisette")
                .join(format!("group-{group}"));
            fs::create_dir_all(&dir).ok();
            dir
        }
    }
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

use super::{Bundle, PlistInfoTrait};
use crate::{Error, SignerApp, SignerOptions, cgbi};
use plist::Dictionary;
use std::path::{Path, PathBuf};
use std::{env, fs, io::Read};
use uuid::Uuid;
use zip::ZipArchive;
use zip::write::FileOptions;

#[derive(Debug, Clone)]
pub struct Package {
    package_file: PathBuf,
    stage_dir: PathBuf,
    info_plist_dictionary: Dictionary,
    archive_entries: Vec<String>,
    pub app_icon_data: Option<Vec<u8>>,
}

impl Package {
    /// Prefix shared by every staging directory.
    const STAGE_PREFIX: &'static str = "plume_stage_";

    /// Deletes staging directories left behind by earlier runs.
    ///
    /// A stage holds a whole copy of the package *and* its whole extraction --
    /// two to three times the size of the IPA. [`Self::remove_package_stage`]
    /// clears it when the installer screen is left, but a crash never reaches
    /// that point, and an install still in flight is deliberately allowed to
    /// leak one rather than have its files deleted mid-sign. Nothing ever
    /// collected those, so every crash left another full copy in the temp
    /// directory permanently. Enough of them fill the disk, and a full disk is
    /// what shows up later as a copy that stops short and an install that fails
    /// looking for a file that was never fully written.
    ///
    /// Only safe while no other instance is running -- a live one is using its
    /// own stage. Call it from startup, once the single-instance lock is held.
    pub fn clean_orphan_stages() {
        let Ok(entries) = fs::read_dir(env::temp_dir()) else {
            return;
        };

        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();

            let is_stage = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(Self::STAGE_PREFIX));

            if !is_stage || !path.is_dir() {
                continue;
            }

            match fs::remove_dir_all(&path) {
                Ok(()) => log::info!("Removed orphaned package stage {}", path.display()),
                Err(e) => log::warn!(
                    "Failed to remove orphaned package stage {}: {e}",
                    path.display()
                ),
            }
        }
    }

    pub fn new(package_file: PathBuf) -> Result<Self, Error> {
        let stage_dir = env::temp_dir().join(format!(
            "{}{:08}",
            Self::STAGE_PREFIX,
            Uuid::new_v4().to_string().to_uppercase()
        ));
        let out_package_file = stage_dir.join("stage.ipa");

        fs::create_dir_all(&stage_dir).ok();

        let source_len = fs::metadata(&package_file)?.len();
        fs::copy(&package_file, &out_package_file)?;

        // What actually landed on disk, not what the copy claimed to write.
        // `fs::copy` reports the source length on Windows, so its return value
        // cannot catch a truncated destination; only the destination can.
        let staged_len = fs::metadata(&out_package_file)?.len();

        // And the source measured again afterwards, because a file still being
        // downloaded or synced grows underneath the copy.
        let source_len_after = fs::metadata(&package_file)?.len();

        // Any disagreement means the copy is not the file: the source was still
        // being written, sits in a cloud folder that had not finished
        // hydrating, or the disk filled up part way through. Carrying on would
        // extract half an archive and fail much later with a confusing
        // missing-file error, so stop here while the cause is still obvious.
        if staged_len != source_len || source_len_after != source_len {
            fs::remove_dir_all(&stage_dir).ok();

            return Err(Error::Other(format!(
                "{} could not be read in full ({source_len} bytes, {staged_len} copied). \
                 If it is still downloading or syncing, wait for it to finish; \
                 if the disk is full, free some space and try again.",
                package_file.display()
            )));
        }

        let file = fs::File::open(&out_package_file)?;
        let mut archive = ZipArchive::new(file)?;
        let archive_entries = (0..archive.len())
            .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
            .collect::<Vec<_>>();

        // What the archive is, on the record. A package that worked yesterday
        // and fails today is the hardest kind of report to act on, because the
        // usual suspects -- a file still syncing, an antivirus that took a
        // bite, a half-finished download, a different build than the one
        // remembered -- all look the same from the outside. With this line in
        // the log, comparing two runs answers it: same digest means the bytes
        // on disk are identical and the fault is ours, a different digest means
        // the file itself changed between the runs.
        log::info!(
            "[package] {} {staged_len} bytes, {} entries, sha256={}",
            package_file.display(),
            archive_entries.len(),
            Self::digest(&out_package_file)?
        );

        // Then every entry read through, so a damaged one is named now rather
        // than surfacing much later as a signing step that cannot find a file.
        Self::verify_archive_contents(&out_package_file)?;

        let info_plist_dictionary =
            Self::get_info_plist_from_archive(&out_package_file, &archive_entries)?;

        let app_icon_data = Self::extract_icon_from_archive(
            &out_package_file,
            &archive_entries,
            &info_plist_dictionary,
        );

        Ok(Self {
            package_file: out_package_file,
            stage_dir: stage_dir.clone(),
            info_plist_dictionary,
            archive_entries,
            app_icon_data,
        })
    }

    /// SHA-256 of a file, hex, read in chunks so a large package does not have
    /// to be held in memory.
    fn digest(path: &Path) -> Result<String, Error> {
        use sha2::{Digest, Sha256};

        let mut file = fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1 << 16];

        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }

        Ok(hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    /// Reads every entry of the archive through, which checks each one against
    /// the checksum the archive itself stores for it.
    ///
    /// A shrunken or partly rewritten `.ipa` passes every cheap test -- it
    /// opens, it lists its entries, the central directory is intact -- and only
    /// gives itself away when something tries to read the bytes. Doing that
    /// here costs a second on a fifty-megabyte package and turns "Core error:
    /// I/O error: The system cannot find the file specified" hours later into
    /// the name of the file that is broken.
    fn verify_archive_contents(archive_path: &Path) -> Result<(), Error> {
        let file = fs::File::open(archive_path)?;
        let mut archive = ZipArchive::new(file)?;

        for index in 0..archive.len() {
            let mut entry = match archive.by_index(index) {
                Ok(entry) => entry,
                Err(e) => {
                    return Err(Error::Other(format!(
                        "entry {index} of the package cannot be opened: {e}.                          The .ipa is damaged -- get a fresh copy of it."
                    )));
                }
            };

            if entry.is_dir() {
                continue;
            }

            let name = entry.name().to_string();

            // The reader checks the entry's stored checksum as it reaches the
            // end, so the bytes only have to be counted, not kept.
            if let Err(e) = std::io::copy(&mut entry, &mut std::io::sink()) {
                return Err(Error::Other(format!(
                    "{name} inside the package is damaged ({e}).                      The .ipa on disk is not the one it should be: it was cut short while                      being written or copied, changed by something else, or scanned and                      altered. Get a fresh copy of the .ipa and try again."
                )));
            }
        }

        Ok(())
    }

    pub fn package_file(&self) -> &PathBuf {
        &self.package_file
    }

    fn get_info_plist_from_archive(
        archive_path: &PathBuf,
        archive_entries: &[String],
    ) -> Result<Dictionary, Error> {
        let file = fs::File::open(archive_path)?;
        let mut archive = ZipArchive::new(file)?;

        let info_plist_path = archive_entries
            .iter()
            .find(|entry| {
                entry.starts_with("Payload/")
                    && entry.ends_with("/Info.plist")
                    && entry.matches('/').count() == 2
            })
            .ok_or(Error::PackageInfoPlistMissing)?;

        let mut plist_file = archive.by_name(info_plist_path)?;
        let mut plist_data = Vec::new();
        plist_file.read_to_end(&mut plist_data)?;

        Ok(plist::from_bytes(&plist_data)?)
    }

    fn extract_icon_from_archive(
        archive_path: &PathBuf,
        archive_entries: &[String],
        plist: &Dictionary,
    ) -> Option<Vec<u8>> {
        // Collects all candidate icon base names from the plist, in order of preference.
        // CFBundleIcons (iPhone) takes priority, fall back to CFBundleIcons~ipad, then
        // top-level CFBundleIconFiles.
        let mut icon_names: Vec<String> = Vec::new();

        let primary_from = |d: &Dictionary| -> Vec<String> {
            d.get("CFBundlePrimaryIcon")
                .and_then(|v| v.as_dictionary())
                .and_then(|d| d.get("CFBundleIconFiles"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_string())
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };

        if let Some(d) = plist.get("CFBundleIcons").and_then(|v| v.as_dictionary()) {
            icon_names.extend(primary_from(d));
        }
        if let Some(d) = plist
            .get("CFBundleIcons~ipad")
            .and_then(|v| v.as_dictionary())
        {
            for n in primary_from(d) {
                if !icon_names.contains(&n) {
                    icon_names.push(n);
                }
            }
        }
        if let Some(arr) = plist.get("CFBundleIconFiles").and_then(|v| v.as_array()) {
            for n in arr
                .iter()
                .filter_map(|v| v.as_string())
                .map(|s| s.to_string())
            {
                if !icon_names.contains(&n) {
                    icon_names.push(n);
                }
            }
        }

        if icon_names.is_empty() {
            return None;
        }

        let app_prefix = archive_entries
            .iter()
            .find(|e| {
                e.starts_with("Payload/")
                    && e.ends_with("/Info.plist")
                    && e.matches('/').count() == 2
            })?
            .trim_end_matches("/Info.plist");

        let file = fs::File::open(archive_path).ok()?;
        let mut archive = ZipArchive::new(file).ok()?;

        let suffixes = ["@3x.png", "@2x.png", "@1x.png", ".png"];

        for name in &icon_names {
            for suffix in &suffixes {
                let candidate = format!("{app_prefix}/{name}{suffix}");
                if let Ok(mut entry) = archive.by_name(&candidate) {
                    let mut data = Vec::new();
                    if entry.read_to_end(&mut data).is_ok() && !data.is_empty() {
                        return Some(cgbi::normalize(data));
                    }
                }
            }
        }

        None
    }

    pub fn get_package_bundle(&self) -> Result<Bundle, Error> {
        self.extract_bundle_into(&self.stage_dir)
    }

    /// A private extraction for one of several installs running at once.
    ///
    /// [`Self::get_package_bundle`] always unpacks into the same directory and
    /// wipes it first, which is right for one install at a time and ruinous for
    /// two: the second would delete the first one's files mid-sign. Each slot
    /// gets its own directory instead. Slots are siblings of the stage rather
    /// than inside it, so exporting the stage never packs them, and they share
    /// the stage prefix, so [`Self::clean_orphan_stages`] collects any a crash
    /// leaves behind.
    pub fn get_package_bundle_in_slot(&self, slot: usize) -> Result<Bundle, Error> {
        let root = self.slot_dir(slot);
        fs::create_dir_all(&root)?;
        self.extract_bundle_into(&root)
    }

    /// Deletes a slot's extraction once its install is done with it; each one
    /// is a whole unpacked copy of the app.
    pub fn remove_bundle_slot(&self, slot: usize) {
        fs::remove_dir_all(self.slot_dir(slot)).ok();
    }

    fn slot_dir(&self, slot: usize) -> PathBuf {
        let mut name = self
            .stage_dir
            .file_name()
            .map(|name| name.to_os_string())
            .unwrap_or_default();
        name.push(format!("_slot{slot}"));
        self.stage_dir.with_file_name(name)
    }

    fn extract_bundle_into(&self, root: &Path) -> Result<Bundle, Error> {
        // Start from a pristine extraction. Signing leaves behind files that are
        // not in the archive at all -- `_CodeSignature`, `embedded.mobileprovision`,
        // injected tweaks -- and extracting over them would carry the previous
        // run's state into this one. That matters now that a package can be
        // installed more than once without being reloaded.
        let payload_dir = root.join("Payload");
        if payload_dir.exists() {
            fs::remove_dir_all(&payload_dir)?;
        }

        let file = fs::File::open(&self.package_file).map_err(|e| {
            Error::Other(format!(
                "cannot open staged package {}: {e}",
                self.package_file.display()
            ))
        })?;
        let mut archive = ZipArchive::new(file)?;
        archive.extract(root)?;

        self.verify_extraction(root)?;

        let app_dir = fs::read_dir(&payload_dir)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.is_dir() && p.extension().and_then(|e| e.to_str()) == Some("app"))
            .ok_or_else(|| Error::PackageInfoPlistMissing)?;

        Ok(Bundle::new(app_dir)?)
    }

    /// Checks that every file the archive lists is actually on disk after
    /// extraction, and names the ones that are not.
    ///
    /// Extraction reports success as soon as it has written everything, but
    /// what is on disk a moment later is not always the same set: antivirus
    /// software scans freshly written executables in the temp directory and
    /// quarantines the ones it dislikes -- sideloaded app binaries trip
    /// heuristics regularly -- and a full disk or a cloud-synced temp folder
    /// can lose files too. Signing then fails deep inside with "The system
    /// cannot find the file specified" and no file named, which reads as a
    /// corrupted IPA when the IPA is fine. Catching it here turns that into a
    /// list of the missing files and the most likely reason.
    fn verify_extraction(&self, root: &Path) -> Result<(), Error> {
        let missing: Vec<&str> = self
            .archive_entries
            .iter()
            .filter(|name| !name.ends_with('/'))
            // Anything present counts, including a directory listed without
            // its trailing slash or a symlink; only a name with nothing behind
            // it is missing.
            .filter(|name| fs::symlink_metadata(root.join(name)).is_err())
            .map(String::as_str)
            .collect();

        if missing.is_empty() {
            return Ok(());
        }

        log::error!(
            "{} of {} files are missing from the extracted package under {}: {:?}",
            missing.len(),
            self.archive_entries.len(),
            root.display(),
            missing
        );

        let shown: Vec<&str> = missing.iter().take(5).copied().collect();
        let more = missing.len().saturating_sub(shown.len());

        Err(Error::Other(format!(
            "{} file(s) vanished from the extracted package right after unpacking: {}{}. \
             This is almost always antivirus quarantining the app's binaries from the temp \
             folder ({}*); add an exclusion for it, or check the disk is not full.",
            missing.len(),
            shown.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            },
            env::temp_dir().join(Self::STAGE_PREFIX).display(),
        )))
    }

    pub fn get_archive_based_on_path(&self, path: &PathBuf) -> Result<PathBuf, Error> {
        if path.is_dir() {
            self.clone().archive_package_bundle()
        } else {
            Ok(self.package_file.clone())
        }
    }

    /// Names the stage holds that belong to us, not to the package.
    const STAGE_ONLY_FILES: [&'static str; 2] = ["stage.ipa", "resigned.ipa"];

    fn archive_package_bundle(self) -> Result<PathBuf, Error> {
        let zip_file_path = self.stage_dir.join("resigned.ipa");
        let file = fs::File::create(&zip_file_path)?;
        let mut zip = zip::ZipWriter::new(file);
        let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        fn add_dir_to_zip(
            zip: &mut zip::ZipWriter<fs::File>,
            path: &PathBuf,
            prefix: &PathBuf,
            options: &FileOptions<'_, zip::write::ExtendedFileOptions>,
        ) -> Result<(), Error> {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let entry_path = entry.path();
                let name = entry_path
                    .strip_prefix(prefix)
                    .map_err(|_| Error::PackageInfoPlistMissing)?
                    .to_string_lossy()
                    .to_string();

                // Our own copies of the archive live beside the extracted files;
                // packing them would nest the package inside itself.
                if Package::STAGE_ONLY_FILES.contains(&name.as_str()) {
                    continue;
                }

                if entry_path.is_file() {
                    zip.start_file(&name, options.clone())?;
                    let mut f = fs::File::open(&entry_path)?;
                    std::io::copy(&mut f, zip)?;
                } else if entry_path.is_dir() {
                    zip.add_directory(&name, options.clone())?;
                    add_dir_to_zip(zip, &entry_path, prefix, options)?;
                }
            }
            Ok(())
        }

        // The whole stage, not just `Payload`. An IPA routinely carries
        // `iTunesMetadata.plist`, `iTunesArtwork`, `Symbols/` and similar
        // alongside it; packing only the payload silently dropped all of them,
        // which is why a re-exported package came back noticeably smaller than
        // the one that went in.
        let stage_dir = self.stage_dir.clone();
        add_dir_to_zip(&mut zip, &stage_dir, &self.stage_dir, &options)?;
        zip.finish()?;

        Ok(zip_file_path)
    }

    pub fn remove_package_stage(self) {
        fs::remove_dir_all(&self.stage_dir).ok();

        // Slots of a multi-install that was cut short sit beside the stage.
        let Some(stage_name) = self.stage_dir.file_name().and_then(|name| name.to_str()) else {
            return;
        };
        let slot_prefix = format!("{stage_name}_slot");
        let Some(parent) = self.stage_dir.parent() else {
            return;
        };
        let Ok(entries) = fs::read_dir(parent) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let is_slot = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&slot_prefix));
            if is_slot {
                fs::remove_dir_all(entry.path()).ok();
            }
        }
    }
}

// TODO: make bundle and package share a common trait for plist info access
macro_rules! get_plist_dict_value {
    ($self:ident, $key:expr) => {{
        $self
            .info_plist_dictionary
            .get($key)
            .and_then(|v| v.as_string())
            .map(|s| s.to_string())
    }};
}

impl PlistInfoTrait for Package {
    fn get_name(&self) -> Option<String> {
        get_plist_dict_value!(self, "CFBundleDisplayName")
            .or_else(|| get_plist_dict_value!(self, "CFBundleName"))
            .or_else(|| self.get_executable())
    }

    fn get_executable(&self) -> Option<String> {
        get_plist_dict_value!(self, "CFBundleExecutable")
    }

    fn get_bundle_identifier(&self) -> Option<String> {
        get_plist_dict_value!(self, "CFBundleIdentifier")
    }

    fn get_bundle_name(&self) -> Option<String> {
        get_plist_dict_value!(self, "CFBundleName")
    }

    fn get_version(&self) -> Option<String> {
        get_plist_dict_value!(self, "CFBundleShortVersionString")
    }

    fn get_build_version(&self) -> Option<String> {
        get_plist_dict_value!(self, "CFBundleVersion")
    }
}

impl Package {
    pub fn load_into_signer_options<'settings, 'slf: 'settings>(
        &'slf self,
        settings: &'settings mut SignerOptions,
    ) {
        let app = if self
            .archive_entries
            .iter()
            .any(|entry| entry.contains("SideStoreApp.framework"))
        {
            SignerApp::LiveContainerAndSideStore
        } else {
            SignerApp::from_bundle_identifier(self.get_bundle_identifier().as_deref())
        };

        let new_settings = SignerOptions::new_for_app(app);
        *settings = new_settings;
    }
}

// Owns immutable process-image capture and lazy running-image diagnostics.
// Does not install/restart binaries or identify an image by a later pathname.
// Split from federation_onboarding_probe.rs after the replacement-image regression.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(
    tag = "status",
    content = "identity",
    rename_all = "camelCase",
    deny_unknown_fields
)]
enum FederationImageObservation {
    Known(FederationImageIdentity),
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationImageIdentity {
    sha256: String,
    file_identity: FederationImageFileIdentity,
    size: u64,
    // Diagnostic path at process start, never the identity or a reopen target.
    path_at_start: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "platform", rename_all = "camelCase", deny_unknown_fields)]
enum FederationImageFileIdentity {
    Windows { volume: u64, index: u64 },
    Unix { dev: u64, inode: u64 },
}

impl FederationImageObservation {
    fn valid_identity(&self) -> bool {
        let Self::Known(image) = self else {
            return false;
        };
        image.size > 0
            && !image.path_at_start.is_empty()
            && image.path_at_start.len() <= 32768
            && !image.path_at_start.chars().any(char::is_control)
            && image.sha256.len() == 64
            && image
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && matches!(
                image.file_identity,
                FederationImageFileIdentity::Windows { index: 1.., .. }
                    | FederationImageFileIdentity::Unix { inode: 1.., .. }
            )
    }

    fn matches_profile(&self, profile: &FederationExecutionProfile, platform: &str) -> bool {
        let Self::Known(image) = self else {
            return false;
        };
        let file_matches = matches!(
            (&image.file_identity, platform),
            (
                FederationImageFileIdentity::Windows { index: 1.., .. },
                "windows"
            ) | (
                FederationImageFileIdentity::Unix { inode: 1.., .. },
                "linux" | "macos"
            )
        );
        file_matches && self.valid_identity() && image.sha256 == profile.binary_sha256
    }
}

struct FederationCapturedImage {
    file: Mutex<fs::File>,
    file_identity: FederationImageFileIdentity,
    size: u64,
    path_at_start: String,
}

struct FederationProcessImage {
    captured: Option<FederationCapturedImage>,
    observation: OnceLock<FederationImageObservation>,
    #[cfg(test)]
    hash_reads: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    fail_hash: AtomicBool,
}

impl FederationProcessImage {
    fn capture(path: Option<PathBuf>) -> Self {
        let captured = (|| {
            let path = path?;
            // Rust's read-only File shares delete on Windows. Retaining this
            // handle permits rename-aside installation and still reads the
            // original image; no later lookup of path_at_start occurs.
            let file = fs::File::open(&path).ok()?;
            let metadata = file.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            Some(FederationCapturedImage {
                file_identity: federation_image_file_identity(&file, &metadata).ok()?,
                size: metadata.len(),
                path_at_start: path.to_string_lossy().into_owned(),
                file: Mutex::new(file),
            })
        })();
        Self {
            captured,
            observation: OnceLock::new(),
            #[cfg(test)]
            hash_reads: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            fail_hash: AtomicBool::new(false),
        }
    }

    fn observe(&self) -> FederationImageObservation {
        self.observation
            .get_or_init(|| {
                let Some(captured) = &self.captured else {
                    return FederationImageObservation::Unknown;
                };
                let result = (|| -> std::io::Result<String> {
                    let mut binary = captured.file.lock().expect("image file mutex poisoned");
                    std::io::Seek::rewind(&mut *binary)?;
                    #[cfg(test)]
                    self.hash_reads.fetch_add(1, Ordering::SeqCst);
                    #[cfg(test)]
                    if self.fail_hash.load(Ordering::SeqCst) {
                        return Err(std::io::Error::other("injected image read failure"));
                    }
                    let mut digest = Sha256::new();
                    let mut buffer = [0_u8; 64 * 1024];
                    loop {
                        let count = std::io::Read::read(&mut *binary, &mut buffer)?;
                        if count == 0 {
                            break;
                        }
                        digest.update(&buffer[..count]);
                    }
                    Ok(format!("{:x}", digest.finalize()))
                })();
                match result {
                    Ok(sha256) => FederationImageObservation::Known(FederationImageIdentity {
                        sha256,
                        file_identity: captured.file_identity.clone(),
                        size: captured.size,
                        path_at_start: captured.path_at_start.clone(),
                    }),
                    Err(_) => FederationImageObservation::Unknown,
                }
            })
            .clone()
    }
}

static FEDERATION_PROCESS_IMAGE: OnceLock<Arc<FederationProcessImage>> = OnceLock::new();

fn capture_federation_process_image() -> Arc<FederationProcessImage> {
    FEDERATION_PROCESS_IMAGE
        .get_or_init(|| {
            Arc::new(FederationProcessImage::capture(
                std::env::current_exe().ok(),
            ))
        })
        .clone()
}

#[cfg(windows)]
fn federation_image_file_identity(
    file: &fs::File,
    _: &fs::Metadata,
) -> std::io::Result<FederationImageFileIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    // SAFETY: the zeroed POD is initialized by the synchronous API, and the
    // borrowed file handle stays live for the call; no ownership is transferred.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(FederationImageFileIdentity::Windows {
        volume: u64::from(info.dwVolumeSerialNumber),
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

#[cfg(unix)]
fn federation_image_file_identity(
    _: &fs::File,
    metadata: &fs::Metadata,
) -> std::io::Result<FederationImageFileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Ok(FederationImageFileIdentity::Unix {
        dev: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(any(unix, windows)))]
fn federation_image_file_identity(
    _: &fs::File,
    _: &fs::Metadata,
) -> std::io::Result<FederationImageFileIdentity> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "image file identity unavailable",
    ))
}

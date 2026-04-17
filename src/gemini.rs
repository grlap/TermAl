// Gemini CLI integration helpers — API-key / Vertex / ADC detection, the
// override settings file that TermAl writes under
// `$TERMAL_DATA_DIR/.gemini-termal/settings.json` to force interactive-shell
// off for headless runs, user/system settings.json lookups, `.env` file
// resolution (home-directory only — workspace .env files are ignored for
// credentials to prevent accidental commit), and dotenv / process-env
// variable lookups used by auth-source detection.
//
// Extracted from runtime.rs into its own `include!()` fragment so runtime.rs
// stays focused on actual runtime processes.

/// Handles Gemini API key missing detail.
fn gemini_api_key_missing_detail() -> String {
    let env_file = find_gemini_env_file()
        .map(|path| display_path_for_user(&path))
        .unwrap_or_else(|| ".env".to_owned());
    format!(
        "Gemini is configured for an API key, but `GEMINI_API_KEY` was not found in the process environment or in {env_file}."
    )
}

/// Handles Gemini API key source.
fn gemini_api_key_source() -> Option<String> {
    env_var_source("GEMINI_API_KEY").or_else(|| dotenv_var_source("GEMINI_API_KEY"))
}

/// Handles Gemini vertex auth source.
fn gemini_vertex_auth_source(workdir: &str) -> Option<String> {
    let vertex_enabled = env_var_present("GOOGLE_GENAI_USE_VERTEXAI")
        || env_var_present("GOOGLE_GENAI_USE_GCA")
        || dotenv_var_present("GOOGLE_GENAI_USE_VERTEXAI")
        || dotenv_var_present("GOOGLE_GENAI_USE_GCA");
    if !vertex_enabled && gemini_selected_auth_type(workdir).as_deref() != Some("vertex-ai") {
        return None;
    }

    if let Some(source) =
        env_var_source("GOOGLE_API_KEY").or_else(|| dotenv_var_source("GOOGLE_API_KEY"))
    {
        return Some(source);
    }

    let has_project =
        env_var_present("GOOGLE_CLOUD_PROJECT") || dotenv_var_present("GOOGLE_CLOUD_PROJECT");
    let has_location =
        env_var_present("GOOGLE_CLOUD_LOCATION") || dotenv_var_present("GOOGLE_CLOUD_LOCATION");
    if has_project && has_location {
        return Some(
            env_var_source("GOOGLE_CLOUD_PROJECT")
                .or_else(|| dotenv_var_source("GOOGLE_CLOUD_PROJECT"))
                .unwrap_or_else(|| "workspace configuration".to_owned()),
        );
    }

    None
}

/// Handles Gemini ADC source.
fn gemini_adc_source() -> Option<String> {
    if let Some(path) = std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        return Some(display_path_for_user(&path));
    }

    let home = home_dir()?;
    let default_path = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).map(|path| {
            path.join("gcloud")
                .join("application_default_credentials.json")
        })
    } else {
        Some(
            home.join(".config")
                .join("gcloud")
                .join("application_default_credentials.json"),
        )
    }?;
    default_path
        .is_file()
        .then(|| display_path_for_user(&default_path))
}

/// Handles Gemini selected auth type.
fn gemini_selected_auth_type(workdir: &str) -> Option<String> {
    let workspace_settings = PathBuf::from(workdir).join(".gemini").join("settings.json");
    // System settings are overrides (highest priority), then user, then project.
    for path in [
        gemini_system_settings_path(),
        gemini_user_settings_path(),
        Some(workspace_settings),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(selected_type) = gemini_selected_auth_type_from_settings_file(&path) {
            return Some(selected_type);
        }
    }
    None
}

/// Prepares a TermAl-managed Gemini system settings override on Windows.
fn prepare_termal_gemini_system_settings(default_workdir: &str) -> Result<Option<PathBuf>> {
    if !cfg!(windows) {
        return Ok(None);
    }

    let target_path = resolve_termal_data_dir(default_workdir)
        .join("gemini-cli")
        .join("system-settings.json");
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let source_path =
        gemini_system_settings_path().filter(|path| path != &target_path && path.is_file());
    let mut settings = load_gemini_settings_json(source_path.as_deref());
    disable_gemini_interactive_shell_in_settings(&mut settings);

    let encoded = serde_json::to_vec_pretty(&settings)
        .context("failed to serialize TermAl Gemini system settings")?;
    let existing = fs::read(&target_path).ok();
    if existing.as_deref() != Some(encoded.as_slice()) {
        fs::write(&target_path, encoded)
            .with_context(|| format!("failed to write `{}`", target_path.display()))?;
    }

    Ok(Some(target_path))
}

/// Loads Gemini settings for the Windows override, ignoring unreadable or malformed input.
fn load_gemini_settings_json(path: Option<&FsPath>) -> Value {
    let Some(path) = path else {
        return json!({});
    };

    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) => {
            eprintln!(
                "termal> ignoring Gemini settings from `{}` while preparing the Windows ACP override: {err}",
                path.display()
            );
            return json!({});
        }
    };
    if raw.trim().is_empty() {
        return json!({});
    }

    match serde_json::from_str(&raw) {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!(
                "termal> ignoring malformed Gemini settings from `{}` while preparing the Windows ACP override: {err}",
                path.display()
            );
            json!({})
        }
    }
}

/// Forces Gemini interactive shell off while preserving other settings.
fn disable_gemini_interactive_shell_in_settings(settings: &mut Value) {
    if !settings.is_object() {
        *settings = json!({});
    }

    let root = settings
        .as_object_mut()
        .expect("Gemini settings should normalize to an object");
    let tools = root.entry("tools".to_owned()).or_insert_with(|| json!({}));
    if !tools.is_object() {
        *tools = json!({});
    }

    let shell = tools
        .as_object_mut()
        .expect("Gemini settings `tools` should normalize to an object")
        .entry("shell".to_owned())
        .or_insert_with(|| json!({}));
    if !shell.is_object() {
        *shell = json!({});
    }

    shell
        .as_object_mut()
        .expect("Gemini settings `tools.shell` should normalize to an object")
        .insert("enableInteractiveShell".to_owned(), Value::Bool(false));
}

/// Returns a non-blocking Windows note when TermAl overrides Gemini interactive shell.
fn gemini_interactive_shell_warning(workdir: &str) -> Option<String> {
    if !cfg!(windows) {
        return None;
    }

    match gemini_interactive_shell_setting(workdir) {
        Some((false, _)) => None,
        Some((true, path)) => Some(format!(
            "TermAl forces Gemini `tools.shell.enableInteractiveShell` to `false` for Windows ACP sessions to avoid PTY startup crashes. The setting in {} is left unchanged.",
            display_path_for_user(&path)
        )),
        None => None,
    }
}

/// Returns the effective Gemini interactive-shell setting and its source file.
fn gemini_interactive_shell_setting(workdir: &str) -> Option<(bool, PathBuf)> {
    let workspace_settings = PathBuf::from(workdir).join(".gemini").join("settings.json");
    // System settings are overrides (highest priority), then user, then project.
    for path in [
        gemini_system_settings_path(),
        gemini_user_settings_path(),
        Some(workspace_settings),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(enabled) = gemini_interactive_shell_setting_from_settings_file(&path) {
            return Some((enabled, path));
        }
    }
    None
}

/// Returns the interactive-shell setting from a Gemini settings file.
fn gemini_interactive_shell_setting_from_settings_file(path: &FsPath) -> Option<bool> {
    let raw = fs::read_to_string(path).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(&raw)
        .ok()?
        .pointer("/tools/shell/enableInteractiveShell")
        .and_then(Value::as_bool)
}

/// Handles Gemini selected auth type from settings file.
fn gemini_selected_auth_type_from_settings_file(path: &FsPath) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(&raw)
        .ok()?
        .pointer("/security/auth/selectedType")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

const GEMINI_DOTENV_ENV_KEYS: &[&str] = &[
    "GEMINI_API_KEY",
    "GOOGLE_GENAI_USE_VERTEXAI",
    "GOOGLE_GENAI_USE_GCA",
    "GOOGLE_API_KEY",
    "GOOGLE_CLOUD_PROJECT",
    "GOOGLE_CLOUD_LOCATION",
];

/// Finds Gemini environment file.
fn find_gemini_env_file() -> Option<PathBuf> {
    let home = home_dir()?;
    let home_gemini_env = home.join(".gemini").join(".env");
    if home_gemini_env.is_file() {
        return Some(home_gemini_env);
    }
    let home_env = home.join(".env");
    home_env.is_file().then_some(home_env)
}

/// Returns the list of Gemini dotenv file paths that exist, in priority order.
fn gemini_env_file_paths() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    [home.join(".gemini").join(".env"), home.join(".env")]
        .into_iter()
        .filter(|p| p.is_file())
        .collect()
}

/// Returns Gemini dotenv env pairs for ACP child launches.
///
/// Each key is resolved independently across all candidate env files so that
/// a user who keeps Gemini flags in `~/.gemini/.env` and API keys in `~/.env`
/// (or vice-versa) gets correct readiness detection and child-env injection.
fn gemini_dotenv_env_pairs() -> Vec<(String, String)> {
    let paths = gemini_env_file_paths();
    if paths.is_empty() {
        return Vec::new();
    }

    GEMINI_DOTENV_ENV_KEYS
        .iter()
        .filter_map(|key| {
            paths
                .iter()
                .find_map(|path| dotenv_file_var_value(path, key))
                .map(|value| ((*key).to_owned(), value))
        })
        .collect()
}

/// Returns the source path of a dotenv variable, searching all candidate files.
fn dotenv_var_source(key: &str) -> Option<String> {
    gemini_env_file_paths()
        .iter()
        .find(|path| dotenv_file_var_value(path, key).is_some())
        .map(|path| display_path_for_user(path))
}

/// Returns whether a dotenv variable is present in any candidate file.
fn dotenv_var_present(key: &str) -> bool {
    dotenv_var_value(key).is_some()
}

/// Returns the value of a dotenv variable, searching all candidate files.
fn dotenv_var_value(key: &str) -> Option<String> {
    gemini_env_file_paths()
        .iter()
        .find_map(|path| dotenv_file_var_value(path, key))
}

fn dotenv_file_var_value(path: &FsPath, key: &str) -> Option<String> {
    let Ok(raw) = fs::read_to_string(path) else {
        return None;
    };
    raw.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return None;
        }
        let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let (name, value) = trimmed.split_once('=')?;
        if name.trim() != key {
            return None;
        }
        let value = value
            .trim()
            .trim_matches(|ch| ch == '"' || ch == '\'')
            .trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn env_var_source(key: &str) -> Option<String> {
    env_var_present(key).then(|| format!("the `{key}` environment variable"))
}

fn env_var_present(key: &str) -> bool {
    std::env::var(key)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

/// Handles Gemini OAuth credentials path.
fn gemini_oauth_credentials_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".gemini").join("oauth_creds.json"))
}

/// Handles Gemini user settings path.
fn gemini_user_settings_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".gemini").join("settings.json"))
}

/// Handles Gemini system settings path.
fn gemini_system_settings_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GEMINI_CLI_SYSTEM_SETTINGS_PATH") {
        return Some(PathBuf::from(path));
    }
    Some(if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/GeminiCli/settings.json")
    } else if cfg!(windows) {
        PathBuf::from("C:\\ProgramData\\gemini-cli\\settings.json")
    } else {
        PathBuf::from("/etc/gemini-cli/settings.json")
    })
}

//! The OnHold desktop and mobile apps. The web app does the work; this shell
//! gives it a window, a safe place for the session token, the device's name,
//! and a way to open pages in the person's own browser (for signing in).

use std::fs;
use std::path::PathBuf;

use tauri::{AppHandle, Manager};

const TOKEN_FILE: &str = "session-token";

/// The token's place in the system keychain, under the app's identifier so
/// that each client's branded app keeps its own.
#[cfg(desktop)]
fn keychain(app: &AppHandle) -> Option<keyring::Entry> {
    keyring::Entry::new(&app.config().identifier, "session").ok()
}

/// The fallback where there is no keychain (some Linux desktops, and the
/// mobile apps, whose data folder only the app itself can read).
fn token_file(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(dir.join(TOKEN_FILE))
}

fn read_file(app: &AppHandle) -> Option<String> {
    let token = fs::read_to_string(token_file(app).ok()?).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

fn write_file(app: &AppHandle, token: &str) -> Result<(), String> {
    let path = token_file(app)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| e.to_string())?;
        file.write_all(token.as_bytes()).map_err(|e| e.to_string())
    }
    #[cfg(not(unix))]
    {
        fs::write(&path, token).map_err(|e| e.to_string())
    }
}

fn remove_file(app: &AppHandle) {
    if let Ok(path) = token_file(app) {
        let _ = fs::remove_file(path);
    }
}

#[tauri::command]
fn token_get(app: AppHandle) -> Option<String> {
    #[cfg(desktop)]
    if let Some(token) = keychain(&app).and_then(|k| k.get_password().ok()) {
        return Some(token);
    }
    read_file(&app)
}

#[tauri::command]
fn token_set(app: AppHandle, token: String) -> Result<(), String> {
    #[cfg(desktop)]
    if keychain(&app).is_some_and(|k| k.set_password(&token).is_ok()) {
        remove_file(&app);
        return Ok(());
    }
    write_file(&app, &token)
}

#[tauri::command]
fn token_clear(app: AppHandle) {
    #[cfg(desktop)]
    if let Some(k) = keychain(&app) {
        let _ = k.delete_credential();
    }
    remove_file(&app);
}

/// Shown to the person when they approve this device, and in their list of sessions.
#[tauri::command]
fn device_name(app: AppHandle) -> String {
    let name = &app.package_info().name;
    let host = gethostname::gethostname()
        .to_string_lossy()
        .trim()
        .to_owned();
    let os = match std::env::consts::OS {
        "macos" => "Mac",
        "windows" => "Windows",
        "linux" => "Linux",
        "ios" => "iOS",
        "android" => "Android",
        other => other,
    };
    if host.is_empty() || host == "localhost" {
        format!("{name} on {os}")
    } else {
        format!("{name} on {host} ({os})")
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            token_get,
            token_set,
            token_clear,
            device_name
        ])
        .run(tauri::generate_context!())
        .expect("the OnHold app could not start");
}

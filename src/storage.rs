use crate::model::AppData;
use std::path::PathBuf;

fn data_file() -> PathBuf {
    let mut dir = dirs::data_dir().unwrap_or_else(std::env::temp_dir);
    dir.push("rustgirl");
    let _ = std::fs::create_dir_all(&dir);
    dir.push("data.json");
    migrate_legacy_data(&dir);
    dir
}

/// One-time migration from the old "postman_clone_rs" data directory (used
/// before the app was renamed to RustGirl) so existing collections aren't lost.
fn migrate_legacy_data(new_path: &std::path::Path) {
    if new_path.exists() {
        return;
    }
    let Some(mut legacy_dir) = dirs::data_dir() else {
        return;
    };
    legacy_dir.push("postman_clone_rs");
    legacy_dir.push("data.json");
    if legacy_dir.exists() {
        let _ = std::fs::copy(&legacy_dir, new_path);
    }
}

pub fn load() -> AppData {
    let path = data_file();
    match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(_) => AppData::default(),
    }
}

pub fn save(data: &AppData) {
    let path = data_file();
    if let Ok(json) = serde_json::to_string_pretty(data) {
        let _ = std::fs::write(path, json);
    }
}

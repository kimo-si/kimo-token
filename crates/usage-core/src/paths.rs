//! App-owned paths shared by the desktop app, CLI, and collector.

use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
const APP_DIR: &str = "dev.kimo-token.bar";
#[cfg(not(target_os = "macos"))]
const APP_DIR: &str = "kimo-token";
const LEGACY_APP_DIR: &str = "tokenme";

// 优先使用新路径，将旧文件或目录原子迁移；迁移失败时继续使用旧数据。
pub fn migrate_path(parent: &Path, name: &str, legacy_name: &str) -> PathBuf {
    let current = parent.join(name);
    let legacy = parent.join(legacy_name);
    if current.exists() || !legacy.exists() {
        return current;
    }
    if std::fs::rename(&legacy, &current).is_ok() || current.exists() {
        return current;
    }
    legacy
}

// 解析平台配置目录，保留设置、SSH 密钥和配额缓存。
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|base| migrate_path(&base, APP_DIR, LEGACY_APP_DIR))
}

// 解析平台数据目录，保留索引数据库及设备同步状态。
pub fn data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|base| migrate_path(&base, APP_DIR, LEGACY_APP_DIR))
}

// 解析平台本地数据目录，保留本机诊断和响应耗时记录。
pub fn local_data_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|base| migrate_path(&base, APP_DIR, LEGACY_APP_DIR))
}

// 解析平台缓存目录，复用已经下载的定价表。
pub fn cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|base| migrate_path(&base, APP_DIR, LEGACY_APP_DIR))
}

// 解析 macOS 日志目录，避免和同名应用共用目录。
#[cfg(target_os = "macos")]
pub fn log_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| migrate_path(&home.join("Library/Logs"), APP_DIR, LEGACY_APP_DIR))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 迁移保留原目录内容，且新旧目录同时存在时不能覆盖已有数据。
    #[test]
    fn migration_keeps_existing_data_and_prefers_the_new_directory() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join(LEGACY_APP_DIR);
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("settings.json"), "previous settings").unwrap();

        let current = migrate_path(root.path(), APP_DIR, LEGACY_APP_DIR);
        assert_eq!(current, root.path().join(APP_DIR));
        assert_eq!(std::fs::read_to_string(current.join("settings.json")).unwrap(), "previous settings");

        std::fs::create_dir(&legacy).unwrap();
        assert_eq!(migrate_path(root.path(), APP_DIR, LEGACY_APP_DIR), current);
        assert!(legacy.is_dir());
    }
}

//! Persistence for proxy channel configurations and caches in `~/.claude/`.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::utils::config::get_config_home;

use super::types::{ChannelConfigFile, ChannelModelsCache, ProxyActiveConfig};

/// Path to `~/.claude/settings_proxy_active.json`.
pub fn active_proxy_path() -> PathBuf {
    get_config_home().join("settings_proxy_active.json")
}

/// Legacy fallback path to `~/.claude/setting_proxy_active.json`.
fn legacy_active_proxy_path() -> PathBuf {
    get_config_home().join("setting_proxy_active.json")
}

/// Path to `~/.claude/settings_<id>.json`.
pub fn channel_config_path(id: &str) -> PathBuf {
    get_config_home().join(format!("settings_{id}.json"))
}

/// Legacy fallback path to `~/.claude/setting_<id>.json`.
fn legacy_channel_config_path(id: &str) -> PathBuf {
    get_config_home().join(format!("setting_{id}.json"))
}

/// Path to `~/.claude/settings_<id>_models.json`.
pub fn channel_models_cache_path(id: &str) -> PathBuf {
    get_config_home().join(format!("settings_{id}_models.json"))
}

/// Legacy fallback path to `~/.claude/setting_<id>_models.json`.
fn legacy_channel_models_cache_path(id: &str) -> PathBuf {
    get_config_home().join(format!("setting_{id}_models.json"))
}

/// Path to `~/.claude/settings_origin.json` (backup of user settings before proxy activation).
pub fn origin_settings_path() -> PathBuf {
    get_config_home().join("settings_origin.json")
}

/// Path to active `~/.claude/settings.json`.
pub fn user_settings_path() -> PathBuf {
    get_config_home().join("settings.json")
}

/// Load the active proxy channel ID from `settings_proxy_active.json`.
pub fn get_active_channel_id() -> Option<String> {
    let path = active_proxy_path();
    let content = fs::read_to_string(&path)
        .or_else(|_| fs::read_to_string(legacy_active_proxy_path()))
        .ok()?;
    let config: ProxyActiveConfig = serde_json::from_str(&content).ok()?;
    config.active.filter(|s| !s.trim().is_empty())
}

/// Set or clear the active proxy channel ID in `settings_proxy_active.json`.
pub fn set_active_channel_id(id: Option<&str>) -> Result<()> {
    let dir = get_config_home();
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create config dir {}", dir.display()))?;

    let config = ProxyActiveConfig {
        active: id.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
    };
    let json = serde_json::to_string_pretty(&config)?;
    fs::write(active_proxy_path(), json)?;
    Ok(())
}

/// Load `settings_<id>.json`.
pub fn load_channel_config(id: &str) -> Option<ChannelConfigFile> {
    let path = channel_config_path(id);
    let content = fs::read_to_string(&path)
        .or_else(|_| fs::read_to_string(legacy_channel_config_path(id)))
        .ok()?;
    serde_json::from_str(&content).ok()
}

/// Save `settings_<id>.json`.
pub fn save_channel_config(id: &str, config: &ChannelConfigFile) -> Result<()> {
    let dir = get_config_home();
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create config dir {}", dir.display()))?;

    let json = serde_json::to_string_pretty(config)?;
    fs::write(channel_config_path(id), json)?;
    Ok(())
}

/// Update specific environment variables in `settings_<id>.json`.
pub fn update_channel_env(id: &str, key_values: &[(&str, &str)]) -> Result<ChannelConfigFile> {
    let mut config = load_channel_config(id).unwrap_or_default();
    for (k, v) in key_values {
        config.env.insert(k.to_string(), v.to_string());
    }
    save_channel_config(id, &config)?;
    Ok(config)
}

/// Load `settings_<id>_models.json`.
pub fn load_channel_models_cache(id: &str) -> Option<ChannelModelsCache> {
    let path = channel_models_cache_path(id);
    let content = fs::read_to_string(&path)
        .or_else(|_| fs::read_to_string(legacy_channel_models_cache_path(id)))
        .ok()?;
    serde_json::from_str(&content).ok()
}

/// Save `settings_<id>_models.json`.
pub fn save_channel_models_cache(id: &str, cache: &ChannelModelsCache) -> Result<()> {
    let dir = get_config_home();
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create config dir {}", dir.display()))?;

    let json = serde_json::to_string_pretty(cache)?;
    fs::write(channel_models_cache_path(id), json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(std::path::PathBuf);
    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("cometix-config-test-{}", uuid::Uuid::new_v4().simple()));
            let _ = std::fs::create_dir_all(&path);
            Self(path)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_active_proxy_round_trip() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let temp = TestDir::new();
        let _guard = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());

        assert_eq!(get_active_channel_id(), None);

        set_active_channel_id(Some("cpa")).unwrap();
        assert_eq!(get_active_channel_id(), Some("cpa".to_string()));

        set_active_channel_id(None).unwrap();
        assert_eq!(get_active_channel_id(), None);
    }

    #[test]
    fn test_channel_config_and_env_update() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let temp = TestDir::new();
        let _guard = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());

        assert!(load_channel_config("cpa").is_none());

        update_channel_env(
            "cpa",
            &[
                ("ANTHROPIC_BASE_URL", "http://127.0.0.1:8317"),
                ("ANTHROPIC_AUTH_TOKEN", "sk-test-token"),
            ],
        )
        .unwrap();

        let loaded = load_channel_config("cpa").expect("should load config");
        assert_eq!(
            loaded.env.get("ANTHROPIC_BASE_URL"),
            Some(&"http://127.0.0.1:8317".to_string())
        );
        assert_eq!(
            loaded.env.get("ANTHROPIC_AUTH_TOKEN"),
            Some(&"sk-test-token".to_string())
        );

        // Verify file is written with plural 'settings_cpa.json'
        assert!(temp.path().join("settings_cpa.json").exists());
    }

    #[test]
    fn test_legacy_setting_fallback() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let temp = TestDir::new();
        let _guard = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());

        // Write legacy files with singular 'setting_'
        let legacy_cpa = temp.path().join("setting_cpa.json");
        std::fs::write(&legacy_cpa, r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:9999"}}"#).unwrap();

        let legacy_active = temp.path().join("setting_proxy_active.json");
        std::fs::write(&legacy_active, r#"{"active":"cpa"}"#).unwrap();

        assert_eq!(get_active_channel_id(), Some("cpa".to_string()));
        let loaded = load_channel_config("cpa").expect("should load from legacy setting_cpa.json");
        assert_eq!(loaded.env.get("ANTHROPIC_BASE_URL"), Some(&"http://127.0.0.1:9999".to_string()));
    }
}

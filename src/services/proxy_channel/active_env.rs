//! Environment variable and settings synchronization for proxy channels.
//!
//! When a proxy channel is activated (e.g. `/provider cpa`), the original `settings.json`
//! is backed up to `settings_origin.json` (if not already backed up), and the channel's
//! configuration is merged into `settings.json`.
//!
//! When deactivating (`/provider off`), `settings_origin.json` is restored back to
//! `settings.json`, leaving no traces and cleanly reverting to native configuration.
//!
//! Switching channels (e.g. `/provider other`) replaces the proxy entries in `settings.json`
//! using `settings_origin.json` as the baseline.

use std::fs;

use anyhow::{Context, Result};
use tracing::info;

use super::config::{
    get_active_channel_id, load_channel_config, origin_settings_path, set_active_channel_id,
    user_settings_path,
};

/// Known proxy-related environment variable keys managed by proxy channels.
pub const PROXY_MANAGED_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_1M_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_1M_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
];

/// Activates a proxy channel by:
/// 1. Backing up original `settings.json` to `settings_origin.json` (if not already present).
/// 2. Merging the channel's `settings_<id>.json` env configuration into `settings.json`.
/// 3. Updating active channel status and refreshing runtime environment variables.
pub fn activate_channel(id: &str) -> Result<()> {
    let origin_path = origin_settings_path();
    let settings_path = user_settings_path();

    if let Some(parent) = origin_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Step 1: Backup settings.json to settings_origin.json if this is the first activation
    if !origin_path.exists() {
        if settings_path.exists() {
            fs::copy(&settings_path, &origin_path).with_context(|| {
                format!(
                    "Failed to backup {} to {}",
                    settings_path.display(),
                    origin_path.display()
                )
            })?;
        } else {
            fs::write(&origin_path, "{}")?;
        }
    }

    // Step 2: Load channel configuration
    let channel_cfg = load_channel_config(id).unwrap_or_default();

    // Step 3: Load baseline settings from settings_origin.json
    let mut base_val: serde_json::Value = if origin_path.exists() {
        fs::read_to_string(&origin_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if !base_val.is_object() {
        base_val = serde_json::json!({});
    }

    // Step 4: Overlay channel environment variables onto base settings
    let obj = base_val.as_object_mut().expect("object verified above");
    let env_val = obj.entry("env").or_insert_with(|| serde_json::json!({}));

    if !env_val.is_object() {
        *env_val = serde_json::json!({});
    }

    if let Some(env_map) = env_val.as_object_mut() {
        // Strip previous proxy-managed keys
        for key in PROXY_MANAGED_ENV_KEYS {
            env_map.remove(*key);
        }
        // Insert new channel keys
        for (k, v) in &channel_cfg.env {
            env_map.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
    }

    // Step 5: Write merged settings to settings.json
    let updated_json = serde_json::to_string_pretty(&base_val)?;
    fs::write(&settings_path, updated_json)?;

    // Step 6: Set active channel ID
    set_active_channel_id(Some(id))?;

    // Step 7: Refresh settings cache and apply to process_env
    crate::utils::settings::settings_cache::reset_settings_cache();
    for key in PROXY_MANAGED_ENV_KEYS {
        crate::utils::process_env::remove(key);
    }
    crate::utils::managed_env::apply_safe_config_environment_variables();

    info!("Activated proxy channel '{id}' and updated settings.json");
    Ok(())
}

/// Deactivates proxy channels by:
/// 1. Restoring `settings_origin.json` back to `settings.json`.
/// 2. Removing `settings_origin.json`.
/// 3. Clearing active channel status and refreshing runtime environment variables.
pub fn deactivate_channel() -> Result<()> {
    let origin_path = origin_settings_path();
    let settings_path = user_settings_path();

    if origin_path.exists() {
        fs::copy(&origin_path, &settings_path).with_context(|| {
            format!(
                "Failed to restore {} to {}",
                origin_path.display(),
                settings_path.display()
            )
        })?;
        let _ = fs::remove_file(&origin_path);
    }

    set_active_channel_id(None)?;

    // Refresh settings cache and clean up proxy environment variables
    crate::utils::settings::settings_cache::reset_settings_cache();
    for key in PROXY_MANAGED_ENV_KEYS {
        crate::utils::process_env::remove(key);
    }
    crate::utils::managed_env::apply_safe_config_environment_variables();

    info!("Deactivated proxy channels and restored original settings.json");
    Ok(())
}

/// Updates environment variables in the active `settings.json` (e.g. when remapping tier models).
pub fn update_active_settings_env(key_values: &[(&str, &str)]) -> Result<()> {
    let settings_path = user_settings_path();
    let mut val: serde_json::Value = if settings_path.exists() {
        fs::read_to_string(&settings_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if !val.is_object() {
        val = serde_json::json!({});
    }

    let obj = val.as_object_mut().expect("object verified above");
    let env_val = obj.entry("env").or_insert_with(|| serde_json::json!({}));
    if !env_val.is_object() {
        *env_val = serde_json::json!({});
    }

    if let Some(env_map) = env_val.as_object_mut() {
        for (k, v) in key_values {
            env_map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
            crate::utils::process_env::set(k, v);
        }
    }

    let updated_json = serde_json::to_string_pretty(&val)?;
    fs::write(&settings_path, updated_json)?;
    crate::utils::settings::settings_cache::reset_settings_cache();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::proxy_channel::config::{load_channel_config, update_channel_env};

    struct TestDir(std::path::PathBuf);
    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("cometix-active-env-test-{}", uuid::Uuid::new_v4().simple()));
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
    fn test_activate_and_deactivate_channel_lifecycle() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let temp = TestDir::new();
        let _guard = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());

        // Setup an initial settings.json
        let initial_settings = serde_json::json!({
            "alwaysApproveResubmit": true,
            "env": {
                "ORIGINAL_VAR": "keep_me"
            }
        });
        std::fs::write(
            temp.path().join("settings.json"),
            serde_json::to_string_pretty(&initial_settings).unwrap(),
        )
        .unwrap();

        // Setup cpa config
        update_channel_env(
            "cpa",
            &[
                ("ANTHROPIC_BASE_URL", "http://127.0.0.1:8317"),
                ("ANTHROPIC_AUTH_TOKEN", "sk-cpa-token"),
                ("ANTHROPIC_DEFAULT_SONNET_MODEL", "gemini-2.5-pro"),
            ],
        )
        .unwrap();

        // 1. Activate CPA
        activate_channel("cpa").unwrap();

        assert_eq!(get_active_channel_id().as_deref(), Some("cpa"));
        assert!(temp.path().join("settings_origin.json").exists());

        // Verify settings.json merged correctly
        let current_content = std::fs::read_to_string(temp.path().join("settings.json")).unwrap();
        let current_json: serde_json::Value = serde_json::from_str(&current_content).unwrap();
        assert_eq!(current_json["alwaysApproveResubmit"], true);
        assert_eq!(current_json["env"]["ORIGINAL_VAR"], "keep_me");
        assert_eq!(
            current_json["env"]["ANTHROPIC_BASE_URL"],
            "http://127.0.0.1:8317"
        );
        assert_eq!(
            current_json["env"]["ANTHROPIC_AUTH_TOKEN"],
            "sk-cpa-token"
        );
        assert_eq!(
            current_json["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"],
            "gemini-2.5-pro"
        );

        // Verify process_env is updated
        assert_eq!(
            crate::utils::process_env::var("ANTHROPIC_BASE_URL").as_deref(),
            Some("http://127.0.0.1:8317")
        );
        assert_eq!(
            crate::utils::process_env::var("ANTHROPIC_AUTH_TOKEN").as_deref(),
            Some("sk-cpa-token")
        );

        // 2. Test tier remapping
        update_active_settings_env(&[("ANTHROPIC_DEFAULT_SONNET_MODEL", "custom-remapped-model")])
            .unwrap();
        let after_remap: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(temp.path().join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            after_remap["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"],
            "custom-remapped-model"
        );

        // 3. Deactivate CPA
        deactivate_channel().unwrap();

        assert_eq!(get_active_channel_id(), None);
        assert!(!temp.path().join("settings_origin.json").exists());

        // Verify settings.json reverted exactly
        let restored_content = std::fs::read_to_string(temp.path().join("settings.json")).unwrap();
        let restored_json: serde_json::Value = serde_json::from_str(&restored_content).unwrap();
        assert_eq!(restored_json["alwaysApproveResubmit"], true);
        assert_eq!(restored_json["env"]["ORIGINAL_VAR"], "keep_me");
        assert!(restored_json["env"].get("ANTHROPIC_BASE_URL").is_none());
        assert!(restored_json["env"]
            .get("ANTHROPIC_DEFAULT_SONNET_MODEL")
            .is_none());

        // Verify process_env cleared proxy keys
        assert_ne!(
            crate::utils::process_env::var("ANTHROPIC_BASE_URL").as_deref(),
            Some("http://127.0.0.1:8317")
        );
    }

    #[test]
    fn test_channel_switching_other_overrides_cpa() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let temp = TestDir::new();
        let _guard = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());

        let initial_settings = serde_json::json!({
            "env": {
                "ORIGINAL_USER_KEY": "user_val"
            }
        });
        std::fs::write(
            temp.path().join("settings.json"),
            serde_json::to_string_pretty(&initial_settings).unwrap(),
        )
        .unwrap();

        update_channel_env(
            "cpa",
            &[
                ("ANTHROPIC_BASE_URL", "http://127.0.0.1:8317"),
                ("ANTHROPIC_DEFAULT_SONNET_MODEL", "cpa-sonnet"),
            ],
        )
        .unwrap();

        update_channel_env(
            "other",
            &[
                ("ANTHROPIC_BASE_URL", "http://127.0.0.1:9000"),
                // other does NOT set ANTHROPIC_DEFAULT_SONNET_MODEL
            ],
        )
        .unwrap();

        // 1. Activate CPA
        activate_channel("cpa").unwrap();
        assert_eq!(get_active_channel_id().as_deref(), Some("cpa"));

        // 2. Switch directly to Other
        activate_channel("other").unwrap();
        assert_eq!(get_active_channel_id().as_deref(), Some("other"));

        // Origin file should still be preserved
        assert!(temp.path().join("settings_origin.json").exists());

        // In settings.json: other's URL applies, cpa-sonnet is removed, original user key remains
        let content = std::fs::read_to_string(temp.path().join("settings.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(json["env"]["ORIGINAL_USER_KEY"], "user_val");
        assert_eq!(json["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:9000");
        assert!(json["env"].get("ANTHROPIC_DEFAULT_SONNET_MODEL").is_none());

        // 3. Deactivate reverts to original
        deactivate_channel().unwrap();
        assert!(!temp.path().join("settings_origin.json").exists());
        let final_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(temp.path().join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(final_json["env"]["ORIGINAL_USER_KEY"], "user_val");
        assert!(final_json["env"].get("ANTHROPIC_BASE_URL").is_none());
    }
}

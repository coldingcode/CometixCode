//! Implementation of `/provider` slash command.

use indexmap::IndexMap;
use tracing::warn;

use crate::commands::Command;
use crate::services::proxy_channel::{
    activate_channel, deactivate_channel, get_active_channel_id, get_channel, load_channel_config,
    save_channel_config, save_channel_models_cache, ChannelConfigFile, ChannelModelsCache,
};
use crate::tool::ToolUseContext;
use crate::types::message::{RenderableMessage, RenderableMessageKind, SystemMessage};
use crate::utils::process_user_input::ProcessUserInputBaseResult;
use uuid::Uuid;

pub const NAME: &str = "provider";
pub const DESCRIPTION: &str =
    "Manage AI proxy channels (e.g. /provider cpa, /provider off, /provider status)";
pub const ARGUMENT_HINT: &str = "[cpa|off|status] [base_url] [auth_token]";

/// Dispatches `/provider` command execution.
pub fn call(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    _context: &ToolUseContext,
) -> ProcessUserInputBaseResult {
    let output = handle_provider_command(args.trim());
    format_command_result(uuid, command.name.as_ref(), args, &output)
}

fn handle_provider_command(args: &str) -> String {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    if tokens.is_empty() || tokens[0].eq_ignore_ascii_case("status") {
        return render_status_view();
    }

    let sub = tokens[0].to_ascii_lowercase();
    match sub.as_str() {
        "off" | "disable" | "none" | "stop" => {
            if let Err(e) = deactivate_channel() {
                return format!("Failed to deactivate proxy channel: {e}");
            }
            "Proxy channels deactivated. Reverted to direct official API configuration.".to_string()
        }
        "refresh" => {
            let Some(active_id) = get_active_channel_id() else {
                return "No active proxy channel to refresh.".to_string();
            };
            let Some(cfg) = load_channel_config(&active_id) else {
                return format!("Active channel '{active_id}' has no configuration file.");
            };
            let base = cfg
                .env
                .get("ANTHROPIC_BASE_URL")
                .cloned()
                .unwrap_or_default();
            let key = cfg
                .env
                .get("ANTHROPIC_AUTH_TOKEN")
                .or_else(|| cfg.env.get("ANTHROPIC_API_KEY"))
                .cloned()
                .unwrap_or_default();
            if let Some(channel) = get_channel(&active_id) {
                if let Ok(ep) = channel.normalize_endpoints(&base) {
                    trigger_background_models_refresh(channel.id().to_string(), ep.clone(), key);
                    return format!(
                        "Triggered model catalog refresh for '{active_id}' ({}), updating in background...",
                        ep.models_url
                    );
                }
            }
            "Failed to refresh models: unable to resolve channel endpoints.".to_string()
        }
        channel_id if tokens.len() == 2 && tokens[1].eq_ignore_ascii_case("refresh") => {
            if let Some(cfg) = load_channel_config(channel_id) {
                let base = cfg
                    .env
                    .get("ANTHROPIC_BASE_URL")
                    .cloned()
                    .unwrap_or_default();
                let key = cfg
                    .env
                    .get("ANTHROPIC_AUTH_TOKEN")
                    .or_else(|| cfg.env.get("ANTHROPIC_API_KEY"))
                    .cloned()
                    .unwrap_or_default();
                if let Some(channel) = get_channel(channel_id) {
                    if let Ok(ep) = channel.normalize_endpoints(&base) {
                        trigger_background_models_refresh(channel.id().to_string(), ep.clone(), key);
                        return format!(
                            "Triggered model catalog refresh for '{channel_id}' ({}), updating in background...",
                            ep.models_url
                        );
                    }
                }
            }
            format!("Failed to refresh models: channel '{channel_id}' is not configured or endpoints are invalid.")
        }
        channel_id => {
            let channel_opt = get_channel(channel_id);
            let display_name = channel_opt.map(|c| c.display_name()).unwrap_or(channel_id);

            // Case A: User supplied connection arguments: `/provider <id> <base_url> [auth_token]`
            if tokens.len() >= 2 {
                let base_url_input = tokens[1];
                let auth_token_input = tokens.get(2).copied().unwrap_or("");

                let endpoints = if let Some(channel) = channel_opt {
                    match channel.normalize_endpoints(base_url_input) {
                        Ok(ep) => ep,
                        Err(e) => return format!("Failed to parse base URL: {e}"),
                    }
                } else {
                    let raw = base_url_input.trim_end_matches('/');
                    let url = if !raw.starts_with("http://") && !raw.starts_with("https://") {
                        format!("http://{raw}")
                    } else {
                        raw.to_string()
                    };
                    crate::services::proxy_channel::types::Endpoints {
                        inference_base_url: url.clone(),
                        models_url: format!("{url}/v1/models"),
                    }
                };

                let mut env = IndexMap::new();
                env.insert(
                    "ANTHROPIC_BASE_URL".to_string(),
                    endpoints.inference_base_url.clone(),
                );
                if !auth_token_input.is_empty() {
                    env.insert("ANTHROPIC_AUTH_TOKEN".to_string(), auth_token_input.to_string());
                }

                // Preserve existing model mappings if present
                if let Some(existing) = load_channel_config(channel_id) {
                    for key in &[
                        "ANTHROPIC_DEFAULT_SONNET_MODEL",
                        "ANTHROPIC_DEFAULT_SONNET_1M_MODEL",
                        "ANTHROPIC_DEFAULT_OPUS_MODEL",
                        "ANTHROPIC_DEFAULT_OPUS_1M_MODEL",
                        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                    ] {
                        if let Some(val) = existing.env.get(*key) {
                            env.insert(key.to_string(), val.clone());
                        }
                    }
                }

                if let Err(e) = save_channel_config(channel_id, &ChannelConfigFile { env }) {
                    return format!("Failed to save channel configuration: {e}");
                }

                if let Err(e) = activate_channel(channel_id) {
                    return format!("Failed to activate channel: {e}");
                }

                // Spawn background model discovery if channel implementation is present
                trigger_background_models_refresh(channel_id.to_string(), endpoints.clone(), auth_token_input.to_string());

                format!(
                    "Proxy channel '{display_name}' ({channel_id}) configured and activated!\n  • Inference Base URL: {}\n  • Syncing model catalog in background...",
                    endpoints.inference_base_url
                )
            } else {
                // Case B: User requested activation: `/provider <id>`
                if let Some(config) = load_channel_config(channel_id) {
                    let base_url = config
                        .env
                        .get("ANTHROPIC_BASE_URL")
                        .cloned()
                        .unwrap_or_else(|| "(not set)".to_string());

                    if let Err(e) = activate_channel(channel_id) {
                        return format!("Failed to activate channel: {e}");
                    }

                    // Trigger background refresh with existing credentials
                    if let Some(channel) = channel_opt {
                        if let Ok(ep) = channel.normalize_endpoints(&base_url) {
                            let key = config
                                .env
                                .get("ANTHROPIC_AUTH_TOKEN")
                                .or_else(|| config.env.get("ANTHROPIC_API_KEY"))
                                .cloned()
                                .unwrap_or_default();
                            trigger_background_models_refresh(channel_id.to_string(), ep, key);
                        }
                    }

                    format!(
                        "Activated proxy channel '{display_name}' ({channel_id}).\n  • Base URL: {base_url}\n  • To remap tier models, use /model."
                    )
                } else {
                    let default_url = channel_opt
                        .map(|c| c.default_base_url())
                        .unwrap_or("http://127.0.0.1:8317");
                    format!(
                        "Channel '{display_name}' ({channel_id}) is not configured yet.\n\nQuick setup & activate syntax:\n  /provider {channel_id} <base_url> <auth_token>\n\nExample:\n  /provider {channel_id} {default_url} your-auth-token"
                    )
                }
            }
        }
    }
}

fn trigger_background_models_refresh(channel_id: String, ep: crate::services::proxy_channel::types::Endpoints, auth_token: String) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            if let Some(channel) = get_channel(&channel_id) {
                match channel.fetch_models(&ep, &auth_token).await {
                    Ok(models) => {
                        let fetched_at = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);

                        let cache = ChannelModelsCache {
                            base_url: ep.inference_base_url.clone(),
                            fetched_at,
                            models,
                        };
                        let _ = save_channel_models_cache(&channel_id, &cache);
                    }
                    Err(err) => {
                        warn!("Background fetch models for '{channel_id}' failed: {err}");
                    }
                }
            }
        });
    }
}

fn render_status_view() -> String {
    let active_id = get_active_channel_id();
    let mut out = String::new();
    out.push_str("=== Proxy Channel Status ===\n");

    match &active_id {
        Some(id) => {
            let channel_name = get_channel(id).map(|c| c.display_name()).unwrap_or(id.as_str());
            out.push_str(&format!("● Active channel: {channel_name} ({id})\n"));
            if let Some(cfg) = load_channel_config(id) {
                let base = cfg.env.get("ANTHROPIC_BASE_URL").map(|s| s.as_str()).unwrap_or("(not set)");
                out.push_str(&format!("  • Inference Base URL: {base}\n"));

                let has_token = cfg.env.contains_key("ANTHROPIC_AUTH_TOKEN") || cfg.env.contains_key("ANTHROPIC_API_KEY");
                out.push_str(&format!("  • Auth Token        : {}\n", if has_token { "(configured)" } else { "(not set)" }));

                let sonnet = cfg.env.get("ANTHROPIC_DEFAULT_SONNET_MODEL").map(|s| s.as_str()).unwrap_or("(default)");
                let sonnet_1m = cfg.env.get("ANTHROPIC_DEFAULT_SONNET_1M_MODEL").map(|s| s.as_str()).unwrap_or("(follows Sonnet[1m])");
                let opus = cfg.env.get("ANTHROPIC_DEFAULT_OPUS_MODEL").map(|s| s.as_str()).unwrap_or("(default)");
                let opus_1m = cfg.env.get("ANTHROPIC_DEFAULT_OPUS_1M_MODEL").map(|s| s.as_str()).unwrap_or("(follows Opus[1m])");
                let haiku = cfg.env.get("ANTHROPIC_DEFAULT_HAIKU_MODEL").map(|s| s.as_str()).unwrap_or("(default)");

                out.push_str("  • Tier Model Mappings:\n");
                out.push_str(&format!("    - Sonnet    : {sonnet}\n"));
                out.push_str(&format!("    - Sonnet 1M : {sonnet_1m}\n"));
                out.push_str(&format!("    - Opus      : {opus}\n"));
                out.push_str(&format!("    - Opus 1M   : {opus_1m}\n"));
                out.push_str(&format!("    - Haiku     : {haiku}\n"));
            }
        }
        None => {
            out.push_str("○ Active channel: Direct (no proxy active)\n");
        }
    }

    out.push_str("\nAvailable commands:\n");
    out.push_str("  /provider cpa                        - Activate CLIProxyAPI (shows setup syntax if unconfigured)\n");
    out.push_str("  /provider cpa <base_url> <auth_token> - Configure and activate CLIProxyAPI\n");
    out.push_str("  /provider refresh                    - Refresh remote model catalog for active channel\n");
    out.push_str("  /provider other                      - Activate generic / other proxy channel\n");
    out.push_str("  /provider off                        - Deactivate all proxies and revert to direct official API\n");
    out.push_str("  /model                               - Select model or press M / Tab to remap tier models\n");

    out
}

fn format_command_result(
    uuid: Option<String>,
    command_name: &str,
    args: &str,
    output: &str,
) -> ProcessUserInputBaseResult {
    use crate::constants::xml::{
        COMMAND_ARGS_TAG, COMMAND_MESSAGE_TAG, COMMAND_NAME_TAG, LOCAL_COMMAND_STDOUT_TAG,
    };

    let input = format!(
        "<{COMMAND_NAME_TAG}>/{command_name}</{COMMAND_NAME_TAG}>\n<{COMMAND_MESSAGE_TAG}>{command_name}</{COMMAND_MESSAGE_TAG}>\n<{COMMAND_ARGS_TAG}>{args}</{COMMAND_ARGS_TAG}>"
    );

    ProcessUserInputBaseResult {
        messages: vec![
            RenderableMessage {
                uuid: uuid.unwrap_or_else(|| Uuid::new_v4().to_string()),
                kind: RenderableMessageKind::System(SystemMessage::local_command(input)),
            },
            RenderableMessage {
                uuid: Uuid::new_v4().to_string(),
                kind: RenderableMessageKind::System(SystemMessage::local_command(format!(
                    "<{LOCAL_COMMAND_STDOUT_TAG}>{output}</{LOCAL_COMMAND_STDOUT_TAG}>"
                ))),
            },
        ],
        should_query: false,
        allowed_tools: None,
        local_action: None,
        query_source: crate::constants::query_source::QuerySource::Prompt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(std::path::PathBuf);
    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("cometix-provider-test-{}", uuid::Uuid::new_v4().simple()));
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
    fn test_handle_provider_off() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let temp = TestDir::new();
        let _guard = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());

        let res = handle_provider_command("off");
        assert!(res.contains("deactivated") || res.contains("Reverted"));
        assert_eq!(get_active_channel_id(), None);
    }

    #[test]
    fn test_handle_provider_status() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let temp = TestDir::new();
        let _guard = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());

        let res = handle_provider_command("");
        assert!(res.contains("Proxy Channel Status"));
        assert!(res.contains("Direct"));
    }

    #[test]
    fn test_handle_provider_configure_and_activate() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let temp = TestDir::new();
        let _guard = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());

        let res = handle_provider_command("cpa http://127.0.0.1:8317 your-token");
        assert!(res.contains("configured and activated"));
        assert_eq!(get_active_channel_id(), Some("cpa".to_string()));

        let cfg = load_channel_config("cpa").expect("config should exist");
        assert_eq!(
            cfg.env.get("ANTHROPIC_BASE_URL"),
            Some(&"http://127.0.0.1:8317".to_string())
        );
        assert_eq!(
            cfg.env.get("ANTHROPIC_AUTH_TOKEN"),
            Some(&"your-token".to_string())
        );
    }
}

//! Resolution of config value specs into concrete values.
//!
//! A spec is `!command` (run it, use trimmed stdout), `env:VAR` (read the
//! environment), or a literal. Used for provider api keys and by the agent for
//! its `env` map.

use anyhow::Context as _;

/// Resolve a value spec into a concrete value. `what` names the setting in
/// error messages (e.g. "api_key" or "env").
pub fn resolve_value_spec(spec: &str, what: &str) -> anyhow::Result<String> {
    if let Some(cmd) = spec.strip_prefix('!') {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            anyhow::bail!("empty {what} command in config");
        }
        let output = std::process::Command::new("sh")
            .args(["-c", cmd])
            .output()
            .with_context(|| format!("failed to run {what} command: {cmd}"))?;
        if !output.status.success() {
            anyhow::bail!(
                "{what} command failed ({status}): {stderr}",
                status = output.status,
                stderr = String::from_utf8_lossy(&output.stderr).trim(),
            );
        }
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if value.is_empty() {
            anyhow::bail!("{what} command produced no output: {cmd}");
        }
        Ok(value)
    } else if let Some(var) = spec.strip_prefix("env:") {
        std::env::var(var.trim())
            .with_context(|| format!("environment variable '{var}' is not set (used for {what})"))
    } else {
        Ok(spec.to_string())
    }
}

/// Resolve an api-key spec.
pub fn resolve_api_key(spec: &str) -> anyhow::Result<String> {
    resolve_value_spec(spec, "api_key")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_literals_and_commands_and_env() {
        assert_eq!(resolve_value_spec("literal-key", "api_key").unwrap(), "literal-key");
        assert_eq!(resolve_api_key("!echo test-key").unwrap(), "test-key");
        // SAFETY: test-only mutation of a dedicated env var.
        unsafe { std::env::set_var("AI_PROVIDERS_KEY_TEST", "env-key") };
        assert_eq!(resolve_api_key("env:AI_PROVIDERS_KEY_TEST").unwrap(), "env-key");
    }

    #[test]
    fn command_failures_are_errors_not_empty_values() {
        assert!(resolve_api_key("!exit 3").is_err());
        assert!(resolve_api_key("!").is_err());
        assert!(resolve_api_key("!true").is_err()); // no output
        assert!(resolve_api_key("env:AI_PROVIDERS_UNSET_VAR_XYZ").is_err());
    }
}

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::process;
use crate::tunnel::TunnelHost;

fn launch_agents_dir() -> Result<PathBuf> {
    let dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join("Library")
        .join("LaunchAgents");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Path to the launchd plist for a tunnel.
pub fn plist_path(name: &str) -> Result<PathBuf> {
    Ok(launch_agents_dir()?.join(format!("com.mole.{}.plist", name)))
}

/// Check if a tunnel has a launchd plist installed.
pub fn is_enabled(name: &str) -> bool {
    plist_path(name).map(|p| p.exists()).unwrap_or(false)
}

/// Generate and install a launchd plist for auto-starting a tunnel.
pub fn enable(tunnel: &TunnelHost) -> Result<()> {
    let log_path = process::log_file(&tunnel.name)?;
    let label = format!("com.mole.{}", tunnel.name);
    let path = plist_path(&tunnel.name)?;

    let autossh = which_autossh()?;

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{autossh}</string>
        <string>-N</string>
        <string>{name}</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>AUTOSSH_PORT</key>
        <string>0</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>StandardOutPath</key>
    <string>/dev/null</string>
</dict>
</plist>"#,
        label = label,
        autossh = autossh,
        name = tunnel.name,
        log = log_path.display(),
    );

    fs::write(&path, plist)
        .with_context(|| format!("failed to write {}", path.display()))?;

    Command::new("launchctl")
        .args(["load", &path.to_string_lossy()])
        .status()
        .context("failed to run launchctl load")?;

    Ok(())
}

/// Remove and unload a launchd plist for a tunnel.
pub fn disable(name: &str) -> Result<()> {
    let path = plist_path(name)?;
    if !path.exists() {
        anyhow::bail!("tunnel '{}' is not enabled for auto-start", name);
    }

    Command::new("launchctl")
        .args(["unload", &path.to_string_lossy()])
        .status()
        .context("failed to run launchctl unload")?;

    fs::remove_file(&path)
        .with_context(|| format!("failed to remove {}", path.display()))?;

    Ok(())
}

/// Find the absolute path to autossh.
fn which_autossh() -> Result<String> {
    let output = Command::new("which")
        .arg("autossh")
        .output()
        .context("failed to run 'which autossh'")?;
    if !output.status.success() {
        anyhow::bail!("autossh not found in PATH");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

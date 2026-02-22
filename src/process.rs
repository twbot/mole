use anyhow::{Context, Result};
use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::health;
use crate::tunnel::TunnelHost;

/// Directory where PID files are stored.
fn pid_dir() -> Result<PathBuf> {
    let dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".mole")
        .join("pids");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn pid_file(name: &str) -> Result<PathBuf> {
    Ok(pid_dir()?.join(format!("{}.pid", name)))
}

/// Directory where tunnel log files are stored.
pub fn log_dir() -> Result<PathBuf> {
    let dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".mole")
        .join("logs");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Path to the log file for a specific tunnel.
pub fn log_file(name: &str) -> Result<PathBuf> {
    Ok(log_dir()?.join(format!("{}.log", name)))
}

/// Check if a process with the given PID is running.
fn is_pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Get process start time from the OS (for adopted processes).
fn get_process_start_epoch(pid: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // Parse the lstart format, e.g. "Thu Feb 13 22:14:05 2026"
    // Simpler approach: use ps -o etime= to get elapsed, subtract from now
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "etime="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let etime = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let elapsed_secs = parse_etime(&etime)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(now.saturating_sub(elapsed_secs))
}

/// Parse ps etime format: [[dd-]hh:]mm:ss
fn parse_etime(s: &str) -> Option<u64> {
    let s = s.trim();
    let (days, rest) = if let Some(pos) = s.find('-') {
        let d: u64 = s[..pos].parse().ok()?;
        (d, &s[pos + 1..])
    } else {
        (0, s)
    };

    let parts: Vec<&str> = rest.split(':').collect();
    let (hours, minutes, seconds) = match parts.len() {
        3 => {
            let h: u64 = parts[0].parse().ok()?;
            let m: u64 = parts[1].parse().ok()?;
            let s: u64 = parts[2].parse().ok()?;
            (h, m, s)
        }
        2 => {
            let m: u64 = parts[0].parse().ok()?;
            let s: u64 = parts[1].parse().ok()?;
            (0, m, s)
        }
        _ => return None,
    };

    Some(days * 86400 + hours * 3600 + minutes * 60 + seconds)
}

/// Write a PID file with format: "<pid>\n<unix_timestamp>"
fn write_pid_file(name: &str, pid: u32, start_time: u64) -> Result<()> {
    let path = pid_file(name)?;
    fs::write(&path, format!("{}\n{}", pid, start_time))?;
    Ok(())
}

/// Read PID and start timestamp from a PID file.
fn read_pid_file(name: &str) -> Result<Option<(u32, Option<u64>)>> {
    let path = pid_file(name)?;
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)?;
    let mut lines = content.lines();
    let pid: u32 = match lines.next().and_then(|l| l.trim().parse().ok()) {
        Some(p) => p,
        None => {
            let _ = fs::remove_file(&path);
            return Ok(None);
        }
    };
    let start_time: Option<u64> = lines.next().and_then(|l| l.trim().parse().ok());
    Ok(Some((pid, start_time)))
}

/// Find a running autossh process for this tunnel via pgrep.
fn find_autossh_pid(name: &str) -> Option<u32> {
    let output = Command::new("pgrep")
        .args(["-f", &format!("autossh.*{}", name)])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().next()?.trim().parse().ok()
}

/// Get the active PID for a tunnel. Checks PID file first, then falls back to pgrep.
/// Adopts externally-started autossh processes by writing a PID file.
pub fn read_pid(name: &str) -> Result<Option<u32>> {
    // First check our PID file
    if let Some((pid, _)) = read_pid_file(name)? {
        if is_pid_alive(pid) {
            return Ok(Some(pid));
        }
        // Stale PID file, clean up
        let _ = fs::remove_file(pid_file(name)?);
    }

    // Fallback: check for autossh processes started outside of mole
    if let Some(pid) = find_autossh_pid(name) {
        // Adopt it — write PID file with process start time from OS
        let start_time = get_process_start_epoch(pid)
            .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs());
        let _ = write_pid_file(name, pid, start_time);
        return Ok(Some(pid));
    }

    Ok(None)
}

/// Get the start time (unix epoch) for an active tunnel.
/// Falls back to querying the OS if the PID file lacks a timestamp.
pub fn get_start_time(name: &str) -> Result<Option<u64>> {
    if let Some((pid, start_time)) = read_pid_file(name)? {
        if is_pid_alive(pid) {
            if let Some(ts) = start_time {
                return Ok(Some(ts));
            }
            // PID file has no timestamp (old format) — look it up and backfill
            if let Some(ts) = get_process_start_epoch(pid) {
                let _ = write_pid_file(name, pid, ts);
                return Ok(Some(ts));
            }
        }
    }
    Ok(None)
}

/// Check if a tunnel is currently active (has a running process).
pub fn is_active(name: &str) -> Result<bool> {
    Ok(read_pid(name)?.is_some())
}

/// Format a duration as a human-readable string like "2h 14m" or "3d 1h".
pub fn format_uptime(start_epoch: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let elapsed = now.saturating_sub(start_epoch);

    let days = elapsed / 86400;
    let hours = (elapsed % 86400) / 3600;
    let minutes = (elapsed % 3600) / 60;

    if days > 0 {
        format!("{}d {}h", days, hours)
    } else if hours > 0 {
        format!("{}h {}m", hours, minutes)
    } else {
        format!("{}m", minutes.max(1))
    }
}

/// If a log file exceeds max_bytes, rename it to .log.old (replacing any
/// previous .old file) so the new run starts with a fresh log.
fn rotate_log(path: &std::path::Path, max_bytes: u64) {
    if let Ok(meta) = fs::metadata(path) {
        if meta.len() > max_bytes {
            let mut old = path.to_path_buf();
            old.set_extension("log.old");
            let _ = fs::rename(path, old);
        }
    }
}

/// Start a tunnel using autossh. Returns the PID of the spawned process.
pub fn start_tunnel(tunnel: &TunnelHost, max_log_bytes: u64) -> Result<u32> {
    if is_active(&tunnel.name)? {
        anyhow::bail!("tunnel '{}' is already active", tunnel.name);
    }

    // Check for port conflicts before spawning
    let mut conflicts = Vec::new();
    for fwd in &tunnel.forwards {
        if !health::is_port_free(fwd.local_port) {
            conflicts.push(fwd.local_port);
        }
    }
    for fwd in &tunnel.dynamic_forwards {
        if !health::is_port_free(fwd.listen_port) {
            conflicts.push(fwd.listen_port);
        }
    }
    if !conflicts.is_empty() {
        let ports: Vec<String> = conflicts.iter().map(|p| p.to_string()).collect();
        anyhow::bail!(
            "local port(s) {} already in use — stop the conflicting process first",
            ports.join(", ")
        );
    }

    let log_path = log_file(&tunnel.name)?;
    rotate_log(&log_path, max_log_bytes);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .context("failed to open log file")?;

    let child = Command::new("autossh")
        .env("AUTOSSH_PORT", "0")
        .arg("-N")
        .arg(&tunnel.name)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .spawn()
        .context("failed to spawn autossh — is it installed?")?;

    let pid = child.id();

    // Brief pause to let autossh fail fast on port conflicts / auth errors
    std::thread::sleep(std::time::Duration::from_secs(1));

    if !is_pid_alive(pid) {
        let _ = fs::remove_file(pid_file(&tunnel.name)?);
        anyhow::bail!(
            "autossh exited immediately — is the port already in use or the host unreachable?"
        );
    }

    let start_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    write_pid_file(&tunnel.name, pid, start_time)?;

    Ok(pid)
}

/// Remove all mole-managed files for a tunnel (PID file, log files).
pub fn cleanup_files(name: &str) -> Result<()> {
    let _ = fs::remove_file(pid_file(name)?);
    let log = log_file(name)?;
    let _ = fs::remove_file(&log);
    let mut log_old = log.clone();
    log_old.set_extension("log.old");
    let _ = fs::remove_file(&log_old);
    Ok(())
}

/// Rename all mole-managed files for a tunnel (PID file, log files).
pub fn rename_files(old_name: &str, new_name: &str) -> Result<()> {
    let old_pid = pid_file(old_name)?;
    if old_pid.exists() {
        fs::rename(&old_pid, pid_file(new_name)?)?;
    }

    let old_log = log_file(old_name)?;
    if old_log.exists() {
        fs::rename(&old_log, log_file(new_name)?)?;
    }

    let mut old_log_old = old_log.clone();
    old_log_old.set_extension("log.old");
    if old_log_old.exists() {
        let mut new_log_old = log_file(new_name)?;
        new_log_old.set_extension("log.old");
        fs::rename(&old_log_old, new_log_old)?;
    }

    Ok(())
}

/// Stop a tunnel by killing its autossh process.
pub fn stop_tunnel(name: &str) -> Result<()> {
    let pid = read_pid(name)?.context(format!("tunnel '{}' is not active", name))?;

    // Send SIGTERM
    let ret = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if ret != 0 {
        anyhow::bail!("failed to kill process {}", pid);
    }

    // Remove PID file
    let path = pid_file(name)?;
    let _ = fs::remove_file(&path);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_etime_mm_ss() {
        assert_eq!(parse_etime("05:30"), Some(330));
    }

    #[test]
    fn parse_etime_hh_mm_ss() {
        assert_eq!(parse_etime("02:14:05"), Some(2 * 3600 + 14 * 60 + 5));
    }

    #[test]
    fn parse_etime_days() {
        assert_eq!(
            parse_etime("3-01:00:00"),
            Some(3 * 86400 + 3600)
        );
    }

    #[test]
    fn parse_etime_with_whitespace() {
        assert_eq!(parse_etime("  10:00  "), Some(600));
    }

    #[test]
    fn parse_etime_invalid() {
        assert_eq!(parse_etime(""), None);
        assert_eq!(parse_etime("abc"), None);
    }

    #[test]
    fn format_uptime_minutes() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let result = format_uptime(now - 120);
        assert_eq!(result, "2m");
    }

    #[test]
    fn format_uptime_hours() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let result = format_uptime(now - 7200);
        assert_eq!(result, "2h 0m");
    }

    #[test]
    fn format_uptime_days() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let result = format_uptime(now - 90000);
        assert_eq!(result, "1d 1h");
    }
}

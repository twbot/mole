use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::tunnel::{DynamicForward, PortForward, RemotePortForward, TunnelHost};

/// Get a list of SSH config files (main config + included files).
fn config_files() -> Result<Vec<PathBuf>> {
    let ssh_dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".ssh");
    let config_path = ssh_dir.join("config");

    if !config_path.exists() {
        anyhow::bail!("~/.ssh/config not found. If you are using a custom SSH config path, set it in ~/.mole/config.toml under ssh_config.");
    }

    let mut files = vec![config_path.clone()];

    let content = fs::read_to_string(&config_path)?;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some((key, value)) = split_directive(trimmed) {
            if key.eq_ignore_ascii_case("include") {
                let expanded = expand_include_path(value, &ssh_dir)?;
                let pattern_str = expanded.to_string_lossy().to_string();
                for entry in glob::glob(&pattern_str).unwrap_or_else(|_| glob::glob("").unwrap()) {
                    if let Ok(path) = entry {
                        if path.is_file() {
                            files.push(path);
                        }
                    }
                }
            }
        }
    }

    Ok(files)
}

fn expand_include_path(pattern: &str, ssh_dir: &Path) -> Result<PathBuf> {
    if pattern.starts_with('~') {
        let home = dirs::home_dir().context("cannot determine home directory")?;
        Ok(home.join(&pattern[2..]))
    } else if pattern.starts_with('/') {
        Ok(PathBuf::from(pattern))
    } else {
        Ok(ssh_dir.join(pattern))
    }
}

/// Find the line range [start, end) of a Host block in a file.
fn find_host_range(path: &Path, name: &str) -> Result<Option<(usize, usize)>> {
    let content = fs::read_to_string(path)?;
    let lines: Vec<&str> = content.lines().collect();

    let mut block_start: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if let Some((key, value)) = split_directive(trimmed) {
            if key.eq_ignore_ascii_case("host") || key.eq_ignore_ascii_case("match") {
                if block_start.is_some() {
                    return Ok(Some((block_start.unwrap(), i)));
                }
                if key.eq_ignore_ascii_case("host") {
                    let host_name = value.split_whitespace().next().unwrap_or("");
                    if host_name == name {
                        block_start = Some(i);
                    }
                }
            }
        }
    }

    if let Some(start) = block_start {
        return Ok(Some((start, lines.len())));
    }

    Ok(None)
}

/// Read a Host block from SSH config without modifying the file.
/// Returns (file_path, block_content) or None if not found.
pub fn read_host_block(name: &str) -> Result<Option<(PathBuf, String)>> {
    let files = config_files()?;
    for file_path in &files {
        if let Some((start, end)) = find_host_range(file_path, name)? {
            let content = fs::read_to_string(file_path)?;
            let lines: Vec<&str> = content.lines().collect();
            let block = lines[start..end].join("\n");
            return Ok(Some((file_path.clone(), block)));
        }
    }
    Ok(None)
}

/// Remove a Host block from the SSH config. Returns the file path it was removed from.
pub fn remove_host_block(name: &str) -> Result<PathBuf> {
    let files = config_files()?;
    for file_path in &files {
        if let Some((start, end)) = find_host_range(file_path, name)? {
            let content = fs::read_to_string(file_path)?;
            let lines: Vec<&str> = content.lines().collect();

            let mut new_lines: Vec<&str> = Vec::new();
            new_lines.extend_from_slice(&lines[..start]);
            new_lines.extend_from_slice(&lines[end..]);

            // Trim trailing blank lines
            while new_lines.last().map_or(false, |l| l.trim().is_empty()) {
                new_lines.pop();
            }

            let mut new_content = new_lines.join("\n");
            if !new_content.is_empty() {
                new_content.push('\n');
            }

            fs::write(file_path, &new_content)?;
            return Ok(file_path.clone());
        }
    }
    anyhow::bail!("Host block '{}' not found in SSH config files", name);
}

/// Rename a Host block in the SSH config. Returns the file path it was found in.
pub fn rename_host_block(old_name: &str, new_name: &str) -> Result<PathBuf> {
    let files = config_files()?;
    for file_path in &files {
        if let Some((start, _end)) = find_host_range(file_path, old_name)? {
            let content = fs::read_to_string(file_path)?;
            let lines: Vec<&str> = content.lines().collect();

            let mut new_lines: Vec<String> = Vec::new();
            for (i, line) in lines.iter().enumerate() {
                if i == start {
                    // Replace the Host line, preserving any leading whitespace
                    let trimmed = line.trim();
                    let leading = &line[..line.len() - trimmed.len()];
                    new_lines.push(format!("{}Host {}", leading, new_name));
                } else {
                    new_lines.push(line.to_string());
                }
            }

            let mut new_content = new_lines.join("\n");
            if content.ends_with('\n') {
                new_content.push('\n');
            }

            fs::write(file_path, &new_content)?;
            return Ok(file_path.clone());
        }
    }
    anyhow::bail!("Host block '{}' not found in SSH config files", old_name);
}

/// Parse ~/.ssh/config (and included files) to find all hosts with LocalForward directives.
pub fn discover_tunnels() -> Result<Vec<TunnelHost>> {
    let ssh_dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".ssh");
    let config_path = ssh_dir.join("config");

    if !config_path.exists() {
        anyhow::bail!("~/.ssh/config not found");
    }

    let mut tunnels = Vec::new();
    parse_file(&config_path, &ssh_dir, &mut tunnels)?;
    Ok(tunnels)
}

fn parse_file(path: &Path, ssh_dir: &Path, tunnels: &mut Vec<TunnelHost>) -> Result<()> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let mut current_host: Option<String> = None;
    let mut current_hostname: Option<String> = None;
    let mut current_forwards: Vec<PortForward> = Vec::new();
    let mut current_remote_forwards: Vec<RemotePortForward> = Vec::new();
    let mut current_dynamic_forwards: Vec<DynamicForward> = Vec::new();
    let mut current_group: Option<String> = None;

    for line in content.lines() {
        let line = line.trim();

        if line.is_empty() {
            continue;
        }

        // Check for group comment inside a Host block before skipping comments
        if line.starts_with('#') {
            if current_host.is_some() {
                if let Some(g) = line.strip_prefix("# mole:group=") {
                    let g = g.trim();
                    if !g.is_empty() {
                        current_group = Some(g.to_string());
                    }
                }
            }
            continue;
        }

        let (key, value) = match split_directive(line) {
            Some(pair) => pair,
            None => continue,
        };

        match key.to_lowercase().as_str() {
            "include" => {
                // Flush current host before processing includes
                flush_host(&mut current_host, &mut current_hostname, &mut current_forwards, &mut current_remote_forwards, &mut current_dynamic_forwards, &mut current_group, tunnels);
                process_include(value, ssh_dir, tunnels)?;
            }
            "host" => {
                // Flush previous host
                flush_host(&mut current_host, &mut current_hostname, &mut current_forwards, &mut current_remote_forwards, &mut current_dynamic_forwards, &mut current_group, tunnels);

                // Skip wildcard patterns
                let name = value.split_whitespace().next().unwrap_or("");
                if !name.contains('*') && !name.contains('?') {
                    current_host = Some(name.to_string());
                }
            }
            "hostname" => {
                if current_host.is_some() {
                    current_hostname = Some(value.to_string());
                }
            }
            "localforward" => {
                if current_host.is_some() {
                    if let Some(fwd) = parse_local_forward(value) {
                        current_forwards.push(fwd);
                    }
                }
            }
            "remoteforward" => {
                if current_host.is_some() {
                    if let Some(fwd) = parse_remote_forward(value) {
                        current_remote_forwards.push(fwd);
                    }
                }
            }
            "dynamicforward" => {
                if current_host.is_some() {
                    if let Some(fwd) = parse_dynamic_forward(value) {
                        current_dynamic_forwards.push(fwd);
                    }
                }
            }
            _ => {}
        }
    }

    // Flush the last host
    flush_host(&mut current_host, &mut current_hostname, &mut current_forwards, &mut current_remote_forwards, &mut current_dynamic_forwards, &mut current_group, tunnels);

    Ok(())
}

fn flush_host(
    host: &mut Option<String>,
    hostname: &mut Option<String>,
    forwards: &mut Vec<PortForward>,
    remote_forwards: &mut Vec<RemotePortForward>,
    dynamic_forwards: &mut Vec<DynamicForward>,
    group: &mut Option<String>,
    tunnels: &mut Vec<TunnelHost>,
) {
    if let Some(name) = host.take() {
        if !forwards.is_empty() || !remote_forwards.is_empty() || !dynamic_forwards.is_empty() {
            tunnels.push(TunnelHost {
                name,
                hostname: hostname.take(),
                forwards: std::mem::take(forwards),
                remote_forwards: std::mem::take(remote_forwards),
                dynamic_forwards: std::mem::take(dynamic_forwards),
                group: group.take(),
            });
        } else {
            *hostname = None;
            *group = None;
        }
    }
    forwards.clear();
    remote_forwards.clear();
    dynamic_forwards.clear();
}

fn split_directive(line: &str) -> Option<(&str, &str)> {
    // SSH config directives can use whitespace or '=' as separator
    let line = line.trim();
    if let Some(eq_pos) = line.find('=') {
        let key = line[..eq_pos].trim();
        let value = line[eq_pos + 1..].trim();
        if !key.is_empty() && !value.is_empty() {
            return Some((key, value));
        }
    }
    let mut parts = line.splitn(2, char::is_whitespace);
    let key = parts.next()?;
    let value = parts.next()?.trim();
    if value.is_empty() {
        return None;
    }
    Some((key, value))
}

fn process_include(pattern: &str, ssh_dir: &Path, tunnels: &mut Vec<TunnelHost>) -> Result<()> {
    let expanded = if pattern.starts_with('~') {
        let home = dirs::home_dir().context("cannot determine home directory")?;
        home.join(&pattern[2..]) // skip "~/"
    } else if pattern.starts_with('/') {
        PathBuf::from(pattern)
    } else {
        ssh_dir.join(pattern)
    };

    let pattern_str = expanded.to_string_lossy().to_string();
    for entry in glob::glob(&pattern_str).unwrap_or_else(|_| glob::glob("").unwrap()) {
        if let Ok(path) = entry {
            if path.is_file() {
                parse_file(&path, ssh_dir, tunnels)?;
            }
        }
    }

    Ok(())
}

/// Parse a LocalForward value like "16443 localhost:6443" or "16443 10.0.0.1:6443"
fn parse_local_forward(value: &str) -> Option<PortForward> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.len() != 2 {
        return None;
    }

    let local_port: u16 = parts[0].parse().ok()?;

    // remote part is host:port
    let remote = parts[1];
    let colon_pos = remote.rfind(':')?;
    let remote_host = &remote[..colon_pos];
    let remote_port: u16 = remote[colon_pos + 1..].parse().ok()?;

    Some(PortForward {
        local_port,
        remote_host: remote_host.to_string(),
        remote_port,
    })
}

/// Parse a DynamicForward value like "1080" or "127.0.0.1:1080"
fn parse_dynamic_forward(value: &str) -> Option<DynamicForward> {
    let value = value.trim();
    // DynamicForward can be just a port or bind_address:port
    if let Some(colon_pos) = value.rfind(':') {
        let port_str = &value[colon_pos + 1..];
        let listen_port: u16 = port_str.parse().ok()?;
        Some(DynamicForward { listen_port })
    } else {
        let listen_port: u16 = value.parse().ok()?;
        Some(DynamicForward { listen_port })
    }
}

/// Parse a RemoteForward value like "9090 localhost:3000"
fn parse_remote_forward(value: &str) -> Option<RemotePortForward> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.len() != 2 {
        return None;
    }

    let bind_port: u16 = parts[0].parse().ok()?;

    let target = parts[1];
    let colon_pos = target.rfind(':')?;
    let remote_host = &target[..colon_pos];
    let remote_port: u16 = target[colon_pos + 1..].parse().ok()?;

    Some(RemotePortForward {
        bind_port,
        remote_host: remote_host.to_string(),
        remote_port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_directive_whitespace() {
        let (k, v) = split_directive("Host my-tunnel").unwrap();
        assert_eq!(k, "Host");
        assert_eq!(v, "my-tunnel");
    }

    #[test]
    fn split_directive_equals() {
        let (k, v) = split_directive("HostName = 10.0.0.1").unwrap();
        assert_eq!(k, "HostName");
        assert_eq!(v, "10.0.0.1");
    }

    #[test]
    fn split_directive_empty() {
        assert!(split_directive("").is_none());
        assert!(split_directive("   ").is_none());
        assert!(split_directive("KeyOnly").is_none());
    }

    #[test]
    fn parse_forward_localhost() {
        let fwd = parse_local_forward("16443 localhost:6443").unwrap();
        assert_eq!(fwd.local_port, 16443);
        assert_eq!(fwd.remote_host, "localhost");
        assert_eq!(fwd.remote_port, 6443);
    }

    #[test]
    fn parse_forward_ip() {
        let fwd = parse_local_forward("8080 10.0.0.1:80").unwrap();
        assert_eq!(fwd.local_port, 8080);
        assert_eq!(fwd.remote_host, "10.0.0.1");
        assert_eq!(fwd.remote_port, 80);
    }

    #[test]
    fn parse_forward_invalid() {
        assert!(parse_local_forward("not_a_port localhost:80").is_none());
        assert!(parse_local_forward("8080").is_none());
        assert!(parse_local_forward("").is_none());
    }

    #[test]
    fn parse_config_block() {
        let dir = std::env::temp_dir();
        let config = dir.join("mole_test_ssh_config");
        std::fs::write(
            &config,
            "Host my-tunnel\n  HostName 10.0.0.1\n  LocalForward 16443 localhost:6443\n",
        )
        .unwrap();

        let mut tunnels = Vec::new();
        parse_file(&config, &dir, &mut tunnels).unwrap();
        std::fs::remove_file(&config).unwrap();

        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].name, "my-tunnel");
        assert_eq!(tunnels[0].hostname.as_deref(), Some("10.0.0.1"));
        assert_eq!(tunnels[0].forwards.len(), 1);
        assert_eq!(tunnels[0].forwards[0].local_port, 16443);
        assert!(tunnels[0].remote_forwards.is_empty());
    }

    #[test]
    fn parse_config_skips_wildcards() {
        let dir = std::env::temp_dir();
        let config = dir.join("mole_test_ssh_wildcard");
        std::fs::write(
            &config,
            "Host *\n  ServerAliveInterval 60\n\nHost dev-*\n  User admin\n",
        )
        .unwrap();

        let mut tunnels = Vec::new();
        parse_file(&config, &dir, &mut tunnels).unwrap();
        std::fs::remove_file(&config).unwrap();

        assert_eq!(tunnels.len(), 0);
    }

    #[test]
    fn find_host_range_middle() {
        let dir = std::env::temp_dir();
        let config = dir.join("mole_test_host_range_mid");
        std::fs::write(
            &config,
            "Host a\n  HostName a.com\n  LocalForward 80 localhost:80\n\nHost b\n  HostName b.com\n  LocalForward 90 localhost:90\n\nHost c\n  HostName c.com\n  LocalForward 100 localhost:100\n",
        )
        .unwrap();

        let range = find_host_range(&config, "b").unwrap();
        assert_eq!(range, Some((4, 8)));

        std::fs::remove_file(&config).unwrap();
    }

    #[test]
    fn find_host_range_first() {
        let dir = std::env::temp_dir();
        let config = dir.join("mole_test_host_range_first");
        std::fs::write(
            &config,
            "Host a\n  HostName a.com\n  LocalForward 80 localhost:80\n\nHost b\n  HostName b.com\n  LocalForward 90 localhost:90\n",
        )
        .unwrap();

        let range = find_host_range(&config, "a").unwrap();
        assert_eq!(range, Some((0, 4)));

        std::fs::remove_file(&config).unwrap();
    }

    #[test]
    fn find_host_range_last() {
        let dir = std::env::temp_dir();
        let config = dir.join("mole_test_host_range_last");
        std::fs::write(
            &config,
            "Host a\n  HostName a.com\n  LocalForward 80 localhost:80\n\nHost b\n  HostName b.com\n  LocalForward 90 localhost:90\n",
        )
        .unwrap();

        let range = find_host_range(&config, "b").unwrap();
        assert_eq!(range, Some((4, 7)));

        std::fs::remove_file(&config).unwrap();
    }

    #[test]
    fn find_host_range_not_found() {
        let dir = std::env::temp_dir();
        let config = dir.join("mole_test_host_range_nf");
        std::fs::write(
            &config,
            "Host a\n  HostName a.com\n",
        )
        .unwrap();

        let range = find_host_range(&config, "missing").unwrap();
        assert_eq!(range, None);

        std::fs::remove_file(&config).unwrap();
    }

    #[test]
    fn parse_config_multiple_tunnels() {
        let dir = std::env::temp_dir();
        let config = dir.join("mole_test_ssh_multi");
        std::fs::write(
            &config,
            "\
Host tunnel-a\n  HostName a.example.com\n  LocalForward 8080 localhost:80\n\
\n\
Host tunnel-b\n  HostName b.example.com\n  LocalForward 9090 localhost:90\n  LocalForward 9091 localhost:91\n",
        )
        .unwrap();

        let mut tunnels = Vec::new();
        parse_file(&config, &dir, &mut tunnels).unwrap();
        std::fs::remove_file(&config).unwrap();

        assert_eq!(tunnels.len(), 2);
        assert_eq!(tunnels[0].name, "tunnel-a");
        assert_eq!(tunnels[0].forwards.len(), 1);
        assert_eq!(tunnels[1].name, "tunnel-b");
        assert_eq!(tunnels[1].forwards.len(), 2);
    }

    #[test]
    fn parse_config_with_group() {
        let dir = std::env::temp_dir();
        let config = dir.join("mole_test_ssh_group");
        std::fs::write(
            &config,
            "# Tunnel: my-tunnel\nHost my-tunnel\n  # mole:group=prod\n  HostName 10.0.0.1\n  LocalForward 8080 localhost:80\n",
        )
        .unwrap();

        let mut tunnels = Vec::new();
        parse_file(&config, &dir, &mut tunnels).unwrap();
        std::fs::remove_file(&config).unwrap();

        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].name, "my-tunnel");
        assert_eq!(tunnels[0].group.as_deref(), Some("prod"));
    }

    #[test]
    fn parse_config_without_group() {
        let dir = std::env::temp_dir();
        let config = dir.join("mole_test_ssh_nogroup");
        std::fs::write(
            &config,
            "Host my-tunnel\n  HostName 10.0.0.1\n  LocalForward 8080 localhost:80\n",
        )
        .unwrap();

        let mut tunnels = Vec::new();
        parse_file(&config, &dir, &mut tunnels).unwrap();
        std::fs::remove_file(&config).unwrap();

        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].group, None);
    }

    #[test]
    fn parse_config_multiple_groups() {
        let dir = std::env::temp_dir();
        let config = dir.join("mole_test_ssh_multigroup");
        std::fs::write(
            &config,
            "\
Host tunnel-a\n  # mole:group=prod\n  HostName a.example.com\n  LocalForward 8080 localhost:80\n\
\n\
Host tunnel-b\n  # mole:group=staging\n  HostName b.example.com\n  LocalForward 9090 localhost:90\n\
\n\
Host tunnel-c\n  HostName c.example.com\n  LocalForward 7070 localhost:70\n",
        )
        .unwrap();

        let mut tunnels = Vec::new();
        parse_file(&config, &dir, &mut tunnels).unwrap();
        std::fs::remove_file(&config).unwrap();

        assert_eq!(tunnels.len(), 3);
        assert_eq!(tunnels[0].group.as_deref(), Some("prod"));
        assert_eq!(tunnels[1].group.as_deref(), Some("staging"));
        assert_eq!(tunnels[2].group, None);
    }

    #[test]
    fn parse_remote_forward_basic() {
        let fwd = parse_remote_forward("9090 localhost:3000").unwrap();
        assert_eq!(fwd.bind_port, 9090);
        assert_eq!(fwd.remote_host, "localhost");
        assert_eq!(fwd.remote_port, 3000);
    }

    #[test]
    fn parse_remote_forward_ip() {
        let fwd = parse_remote_forward("8080 10.0.0.1:80").unwrap();
        assert_eq!(fwd.bind_port, 8080);
        assert_eq!(fwd.remote_host, "10.0.0.1");
        assert_eq!(fwd.remote_port, 80);
    }

    #[test]
    fn parse_remote_forward_invalid() {
        assert!(parse_remote_forward("not_a_port localhost:80").is_none());
        assert!(parse_remote_forward("8080").is_none());
        assert!(parse_remote_forward("").is_none());
    }

    #[test]
    fn parse_config_remote_forward_only() {
        let dir = std::env::temp_dir();
        let config = dir.join("mole_test_ssh_remote_only");
        std::fs::write(
            &config,
            "Host reverse-tunnel\n  HostName bastion.example.com\n  RemoteForward 9090 localhost:3000\n",
        )
        .unwrap();

        let mut tunnels = Vec::new();
        parse_file(&config, &dir, &mut tunnels).unwrap();
        std::fs::remove_file(&config).unwrap();

        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].name, "reverse-tunnel");
        assert!(tunnels[0].forwards.is_empty());
        assert_eq!(tunnels[0].remote_forwards.len(), 1);
        assert_eq!(tunnels[0].remote_forwards[0].bind_port, 9090);
        assert_eq!(tunnels[0].remote_forwards[0].remote_host, "localhost");
        assert_eq!(tunnels[0].remote_forwards[0].remote_port, 3000);
    }

    #[test]
    fn parse_config_mixed_forwards() {
        let dir = std::env::temp_dir();
        let config = dir.join("mole_test_ssh_mixed_fwd");
        std::fs::write(
            &config,
            "Host mixed-tunnel\n  HostName 10.0.0.1\n  LocalForward 8080 localhost:6443\n  RemoteForward 9090 localhost:3000\n",
        )
        .unwrap();

        let mut tunnels = Vec::new();
        parse_file(&config, &dir, &mut tunnels).unwrap();
        std::fs::remove_file(&config).unwrap();

        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].forwards.len(), 1);
        assert_eq!(tunnels[0].remote_forwards.len(), 1);
        assert_eq!(tunnels[0].forwards[0].local_port, 8080);
        assert_eq!(tunnels[0].remote_forwards[0].bind_port, 9090);
    }

    #[test]
    fn parse_dynamic_forward_port_only() {
        let fwd = parse_dynamic_forward("1080").unwrap();
        assert_eq!(fwd.listen_port, 1080);
    }

    #[test]
    fn parse_dynamic_forward_with_bind_address() {
        let fwd = parse_dynamic_forward("127.0.0.1:1080").unwrap();
        assert_eq!(fwd.listen_port, 1080);
    }

    #[test]
    fn parse_dynamic_forward_invalid() {
        assert!(parse_dynamic_forward("not_a_port").is_none());
        assert!(parse_dynamic_forward("").is_none());
    }

    #[test]
    fn parse_config_dynamic_forward_only() {
        let dir = std::env::temp_dir();
        let config = dir.join("mole_test_ssh_dynamic_only");
        std::fs::write(
            &config,
            "Host socks-proxy\n  HostName bastion.example.com\n  DynamicForward 1080\n",
        )
        .unwrap();

        let mut tunnels = Vec::new();
        parse_file(&config, &dir, &mut tunnels).unwrap();
        std::fs::remove_file(&config).unwrap();

        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].name, "socks-proxy");
        assert!(tunnels[0].forwards.is_empty());
        assert!(tunnels[0].remote_forwards.is_empty());
        assert_eq!(tunnels[0].dynamic_forwards.len(), 1);
        assert_eq!(tunnels[0].dynamic_forwards[0].listen_port, 1080);
    }

    #[test]
    fn parse_config_mixed_all_forward_types() {
        let dir = std::env::temp_dir();
        let config = dir.join("mole_test_ssh_all_fwd_types");
        std::fs::write(
            &config,
            "Host all-types\n  HostName 10.0.0.1\n  LocalForward 8080 localhost:80\n  RemoteForward 9090 localhost:3000\n  DynamicForward 1080\n",
        )
        .unwrap();

        let mut tunnels = Vec::new();
        parse_file(&config, &dir, &mut tunnels).unwrap();
        std::fs::remove_file(&config).unwrap();

        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].forwards.len(), 1);
        assert_eq!(tunnels[0].remote_forwards.len(), 1);
        assert_eq!(tunnels[0].dynamic_forwards.len(), 1);
        assert_eq!(tunnels[0].dynamic_forwards[0].listen_port, 1080);
    }
}

mod cli;
mod config;
mod display;
mod health;
mod launchd;
mod picker;
mod process;
mod ssh_config;
mod tunnel;
mod wizard;

use std::time::Duration;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use colored::Colorize;

use cli::{Cli, Command};
use config::Config;

fn main() -> Result<()> {
    clap_complete::CompleteEnv::with_factory(Cli::command).complete();

    let cli = Cli::parse();
    let cfg = Config::load();

    if cli.no_color {
        colored::control::set_override(false);
    }

    match cli.command {
        Command::Up {
            name,
            all,
            group,
            persist,
        } => cmd_up(name, all, group, persist, &cfg),
        Command::Down { name, all, group } => cmd_down(name, all, group),
        Command::Remove { name } => cmd_remove(name),
        Command::Rename { old, new_name } => cmd_rename(old, new_name),
        Command::Restart { name, all, group } => cmd_restart(name, all, group, &cfg),
        Command::List { group } => cmd_list(group),
        Command::Check => cmd_check(),
        Command::Add => wizard::cmd_add(),
        Command::Edit => cmd_edit(&cfg),
        Command::Logs {
            name,
            lines,
            follow,
        } => cmd_logs(name, lines, follow),
        Command::Enable { name, group } => cmd_enable(name, group),
        Command::Disable { name, group } => cmd_disable(name, group),
        Command::Config => cmd_config(&cfg),
        Command::Completions { shell } => cmd_completions(shell, &cfg),
        Command::ListTunnelNames => cmd_list_tunnel_names(),
    }
}

/// Build a display string for all forwards (local + remote + dynamic) of a tunnel.
fn format_all_forwards(t: &tunnel::TunnelHost) -> String {
    let mut parts: Vec<String> = t.forwards.iter().map(|f| f.to_string()).collect();
    parts.extend(t.remote_forwards.iter().map(|f| f.to_string()));
    parts.extend(t.dynamic_forwards.iter().map(|f| f.to_string()));
    parts.join(", ")
}

fn print_start_status(name: &str, pid: u32, tunnel: &tunnel::TunnelHost, cfg: &Config) {
    let local_ports: Vec<u16> = tunnel
        .forwards
        .iter()
        .map(|f| f.local_port)
        .chain(tunnel.dynamic_forwards.iter().map(|f| f.listen_port))
        .collect();

    if local_ports.is_empty() {
        // Remote-only tunnel — can't probe health
        println!(
            "{} {} {} (pid {})",
            "●".green(),
            name.green().bold(),
            "started".green(),
            pid,
        );
        return;
    }
    let timeout = Duration::from_secs(cfg.health_timeout);
    let healthy = health::wait_healthy_ports(&local_ports, timeout);
    let health_msg = if healthy {
        format!("{} healthy", "✓".green())
    } else {
        format!("{} port not reachable yet", "✗".yellow())
    };
    println!(
        "{} {} {} (pid {}) — {}",
        "●".green(),
        name.green().bold(),
        "started".green(),
        pid,
        health_msg
    );
}

fn tunnels_in_group<'a>(tunnels: &'a [tunnel::TunnelHost], group: &str) -> Vec<&'a tunnel::TunnelHost> {
    tunnels
        .iter()
        .filter(|t| t.group.as_deref() == Some(group))
        .collect()
}

fn cmd_up(name: Option<String>, all: bool, group: Option<String>, persist: bool, cfg: &Config) -> Result<()> {
    let tunnels = ssh_config::discover_tunnels()?;

    if all {
        let inactive: Vec<&tunnel::TunnelHost> = tunnels
            .iter()
            .filter(|t| !process::is_active(&t.name).unwrap_or(false))
            .collect();

        if inactive.is_empty() {
            println!("{}", "All tunnels are already active.".yellow());
            return Ok(());
        }

        for t in &inactive {
            match process::start_tunnel(t, cfg.max_log_size) {
                Ok(pid) => {
                    print_start_status(&t.name, pid, t, cfg);
                    if persist {
                        if let Err(e) = launchd::enable(t) {
                            println!(
                                "  {} failed to enable auto-start: {}",
                                "⚠".yellow(),
                                e
                            );
                        }
                    }
                }
                Err(e) => {
                    println!(
                        "{} {} — {}",
                        "✗".red(),
                        t.name.red().bold(),
                        e
                    );
                }
            }
        }
        return Ok(());
    }

    if let Some(ref group) = group {
        let in_group = tunnels_in_group(&tunnels, group);
        if in_group.is_empty() {
            anyhow::bail!("no tunnels found in group '{}'", group);
        }

        let inactive: Vec<&&tunnel::TunnelHost> = in_group
            .iter()
            .filter(|t| !process::is_active(&t.name).unwrap_or(false))
            .collect();

        if inactive.is_empty() {
            println!("{}", format!("All tunnels in group '{}' are already active.", group).yellow());
            return Ok(());
        }

        for t in &inactive {
            match process::start_tunnel(t, cfg.max_log_size) {
                Ok(pid) => {
                    print_start_status(&t.name, pid, t, cfg);
                    if persist {
                        if let Err(e) = launchd::enable(t) {
                            println!(
                                "  {} failed to enable auto-start: {}",
                                "⚠".yellow(),
                                e
                            );
                        }
                    }
                }
                Err(e) => {
                    println!(
                        "{} {} — {}",
                        "✗".red(),
                        t.name.red().bold(),
                        e
                    );
                }
            }
        }
        return Ok(());
    }

    let tunnel = match name {
        Some(ref n) => tunnels
            .iter()
            .find(|t| t.name == *n)
            .ok_or_else(|| anyhow::anyhow!("tunnel '{}' not found in SSH config", n))?,
        None => {
            let inactive: Vec<&tunnel::TunnelHost> = tunnels
                .iter()
                .filter(|t| !process::is_active(&t.name).unwrap_or(false))
                .collect();

            if inactive.is_empty() {
                println!("{}", "All tunnels are already active.".yellow());
                return Ok(());
            }

            let items: Vec<String> = inactive
                .iter()
                .map(|t| format!("{} ({})", t.name, format_all_forwards(t)))
                .collect();

            let idx = picker::pick("Start tunnel", &items)?;
            inactive[idx]
        }
    };

    if process::is_active(&tunnel.name)? {
        println!("{} is already active", tunnel.name.yellow());
        return Ok(());
    }

    let pid = process::start_tunnel(tunnel, cfg.max_log_size)?;
    print_start_status(&tunnel.name, pid, tunnel, cfg);

    if persist {
        match launchd::enable(tunnel) {
            Ok(()) => println!(
                "  {} auto-start enabled",
                "⏎".green()
            ),
            Err(e) => println!(
                "  {} failed to enable auto-start: {}",
                "⚠".yellow(),
                e
            ),
        }
    }

    Ok(())
}

fn cmd_down(name: Option<String>, all: bool, group: Option<String>) -> Result<()> {
    let tunnels = ssh_config::discover_tunnels()?;

    if all {
        let active: Vec<&tunnel::TunnelHost> = tunnels
            .iter()
            .filter(|t| process::is_active(&t.name).unwrap_or(false))
            .collect();

        if active.is_empty() {
            println!("{}", "No active tunnels.".yellow());
            return Ok(());
        }

        for t in &active {
            match process::stop_tunnel(&t.name) {
                Ok(()) => println!(
                    "{} {} {}",
                    "○".dimmed(),
                    t.name.bold(),
                    "stopped".dimmed()
                ),
                Err(e) => println!(
                    "{} {} — {}",
                    "✗".red(),
                    t.name.red().bold(),
                    e
                ),
            }
        }
        return Ok(());
    }

    if let Some(ref group) = group {
        let in_group = tunnels_in_group(&tunnels, group);
        if in_group.is_empty() {
            anyhow::bail!("no tunnels found in group '{}'", group);
        }

        let active: Vec<&&tunnel::TunnelHost> = in_group
            .iter()
            .filter(|t| process::is_active(&t.name).unwrap_or(false))
            .collect();

        if active.is_empty() {
            println!("{}", format!("No active tunnels in group '{}'.", group).yellow());
            return Ok(());
        }

        for t in &active {
            match process::stop_tunnel(&t.name) {
                Ok(()) => println!(
                    "{} {} {}",
                    "○".dimmed(),
                    t.name.bold(),
                    "stopped".dimmed()
                ),
                Err(e) => println!(
                    "{} {} — {}",
                    "✗".red(),
                    t.name.red().bold(),
                    e
                ),
            }
        }
        return Ok(());
    }

    let tunnel_name = match name {
        Some(n) => {
            if !tunnels.iter().any(|t| t.name == n) {
                anyhow::bail!("tunnel '{}' not found in SSH config", n);
            }
            n
        }
        None => {
            let active: Vec<&tunnel::TunnelHost> = tunnels
                .iter()
                .filter(|t| process::is_active(&t.name).unwrap_or(false))
                .collect();

            if active.is_empty() {
                println!("{}", "No active tunnels.".yellow());
                return Ok(());
            }

            let items: Vec<String> = active
                .iter()
                .map(|t| format!("{} ({})", t.name, format_all_forwards(t)))
                .collect();

            let idx = picker::pick("Stop tunnel", &items)?;
            active[idx].name.clone()
        }
    };

    if !process::is_active(&tunnel_name)? {
        println!("{} is not active", tunnel_name.yellow());
        return Ok(());
    }

    process::stop_tunnel(&tunnel_name)?;
    println!(
        "{} {} {}",
        "○".dimmed(),
        tunnel_name.bold(),
        "stopped".dimmed()
    );

    Ok(())
}

fn cmd_remove(name: Option<String>) -> Result<()> {
    let tunnels = ssh_config::discover_tunnels()?;

    let tunnel = match name {
        Some(ref n) => tunnels
            .iter()
            .find(|t| t.name == *n)
            .ok_or_else(|| anyhow::anyhow!("tunnel '{}' not found in SSH config", n))?,
        None => {
            let items: Vec<String> = tunnels
                .iter()
                .map(|t| format!("{} ({})", t.name, format_all_forwards(t)))
                .collect();

            if items.is_empty() {
                println!("{}", "No tunnels found.".yellow());
                return Ok(());
            }

            let idx = picker::pick("Remove tunnel", &items)?;
            &tunnels[idx]
        }
    };

    // Show what will be removed
    if let Ok(Some((_path, block))) = ssh_config::read_host_block(&tunnel.name) {
        println!("{}", "Will remove from SSH config:".dimmed());
        for line in block.lines() {
            println!("  {}", line.dimmed());
        }
        println!();
    }

    let confirmed = dialoguer::Confirm::new()
        .with_prompt(format!("Remove {}?", tunnel.name))
        .default(false)
        .interact()
        .context("failed to read confirmation")?;

    if !confirmed {
        println!("Cancelled.");
        return Ok(());
    }

    // Stop if active
    if process::is_active(&tunnel.name)? {
        process::stop_tunnel(&tunnel.name)?;
        println!(
            "{} {} {}",
            "○".dimmed(),
            tunnel.name.bold(),
            "stopped".dimmed()
        );
    }

    // Disable launchd if enabled
    if launchd::is_enabled(&tunnel.name) {
        launchd::disable(&tunnel.name)?;
        println!(
            "{} auto-start {}",
            "○".dimmed(),
            "disabled".dimmed()
        );
    }

    // Remove from SSH config
    let file_path = ssh_config::remove_host_block(&tunnel.name)?;

    // Clean up mole files
    process::cleanup_files(&tunnel.name)?;

    println!(
        "{} {} removed from {}",
        "✓".green(),
        tunnel.name.green().bold(),
        file_path.display()
    );

    Ok(())
}

fn cmd_rename(old: Option<String>, new_name: String) -> Result<()> {
    let tunnels = ssh_config::discover_tunnels()?;

    let old_name = match old {
        Some(n) => {
            if !tunnels.iter().any(|t| t.name == n) {
                anyhow::bail!("tunnel '{}' not found in SSH config", n);
            }
            n
        }
        None => {
            let items: Vec<String> = tunnels
                .iter()
                .map(|t| format!("{} ({})", t.name, format_all_forwards(t)))
                .collect();

            if items.is_empty() {
                println!("{}", "No tunnels found.".yellow());
                return Ok(());
            }

            let idx = picker::pick("Rename tunnel", &items)?;
            tunnels[idx].name.clone()
        }
    };

    if tunnels.iter().any(|t| t.name == new_name) {
        anyhow::bail!("tunnel '{}' already exists", new_name);
    }

    // Stop if active
    let was_active = process::is_active(&old_name)?;
    if was_active {
        process::stop_tunnel(&old_name)?;
        println!(
            "{} {} {}",
            "○".dimmed(),
            old_name.bold(),
            "stopped".dimmed()
        );
    }

    // Disable launchd if enabled
    let was_enabled = launchd::is_enabled(&old_name);
    if was_enabled {
        launchd::disable(&old_name)?;
    }

    // Rename SSH config host block
    ssh_config::rename_host_block(&old_name, &new_name)?;

    // Rename mole-managed files (PID, logs)
    process::rename_files(&old_name, &new_name)?;

    // Re-enable launchd if it was enabled
    if was_enabled {
        let tunnels = ssh_config::discover_tunnels()?;
        let new_tunnel = tunnels
            .iter()
            .find(|t| t.name == new_name)
            .ok_or_else(|| anyhow::anyhow!("renamed tunnel '{}' not found after rename", new_name))?;
        launchd::enable(new_tunnel)?;
    }

    println!(
        "{} renamed {} -> {}",
        "✓".green(),
        old_name.green().bold(),
        new_name.green().bold()
    );

    Ok(())
}

fn restart_tunnel(tunnel: &tunnel::TunnelHost, cfg: &Config) -> Result<()> {
    if process::is_active(&tunnel.name)? {
        process::stop_tunnel(&tunnel.name)?;
        println!(
            "{} {} {}",
            "○".dimmed(),
            tunnel.name.bold(),
            "stopped".dimmed()
        );
    }

    let pid = process::start_tunnel(tunnel, cfg.max_log_size)?;
    print_start_status(&tunnel.name, pid, tunnel, cfg);
    Ok(())
}

fn cmd_restart(name: Option<String>, all: bool, group: Option<String>, cfg: &Config) -> Result<()> {
    let tunnels = ssh_config::discover_tunnels()?;

    if all {
        let active: Vec<&tunnel::TunnelHost> = tunnels
            .iter()
            .filter(|t| process::is_active(&t.name).unwrap_or(false))
            .collect();

        if active.is_empty() {
            println!("{}", "No active tunnels to restart.".yellow());
            return Ok(());
        }

        for t in &active {
            if let Err(e) = restart_tunnel(t, cfg) {
                println!(
                    "{} {} — {}",
                    "✗".red(),
                    t.name.red().bold(),
                    e
                );
            }
        }
        return Ok(());
    }

    if let Some(ref group) = group {
        let in_group = tunnels_in_group(&tunnels, group);
        if in_group.is_empty() {
            anyhow::bail!("no tunnels found in group '{}'", group);
        }

        for t in &in_group {
            if let Err(e) = restart_tunnel(t, cfg) {
                println!(
                    "{} {} — {}",
                    "✗".red(),
                    t.name.red().bold(),
                    e
                );
            }
        }
        return Ok(());
    }

    let tunnel = match name {
        Some(ref n) => tunnels
            .iter()
            .find(|t| t.name == *n)
            .ok_or_else(|| anyhow::anyhow!("tunnel '{}' not found in SSH config", n))?,
        None => {
            let active: Vec<&tunnel::TunnelHost> = tunnels
                .iter()
                .filter(|t| process::is_active(&t.name).unwrap_or(false))
                .collect();

            if active.is_empty() {
                println!("{}", "No active tunnels to restart.".yellow());
                return Ok(());
            }

            let items: Vec<String> = active
                .iter()
                .map(|t| format!("{} ({})", t.name, format_all_forwards(t)))
                .collect();

            let idx = picker::pick("Restart tunnel", &items)?;
            active[idx]
        }
    };

    restart_tunnel(tunnel, cfg)?;

    Ok(())
}

fn cmd_list(group: Option<String>) -> Result<()> {
    let tunnels = ssh_config::discover_tunnels()?;
    if let Some(ref group) = group {
        let filtered: Vec<tunnel::TunnelHost> = tunnels
            .into_iter()
            .filter(|t| t.group.as_deref() == Some(group.as_str()))
            .collect();
        if filtered.is_empty() {
            anyhow::bail!("no tunnels found in group '{}'", group);
        }
        display::print_tunnel_list(&filtered);
    } else {
        display::print_tunnel_list(&tunnels);
    }
    Ok(())
}

fn cmd_check() -> Result<()> {
    let tunnels = ssh_config::discover_tunnels()?;

    let active: Vec<&tunnel::TunnelHost> = tunnels
        .iter()
        .filter(|t| process::is_active(&t.name).unwrap_or(false))
        .collect();

    if active.is_empty() {
        println!("{}", "No active tunnels to check.".yellow());
        return Ok(());
    }

    let mut total_ports = 0;
    let mut healthy_ports = 0;

    for t in &active {
        let mut all_ok = true;
        print!("  {} {:<20}", "●".green(), t.name.green().bold());

        for fwd in &t.forwards {
            total_ports += 1;
            let ok = health::check_port(fwd.local_port);
            if ok {
                healthy_ports += 1;
            } else {
                all_ok = false;
            }
            let icon = if ok {
                "✓".green().to_string()
            } else {
                "✗".red().to_string()
            };
            print!("  {} :{}", icon, fwd.local_port);
        }

        for fwd in &t.dynamic_forwards {
            total_ports += 1;
            let ok = health::check_port(fwd.listen_port);
            if ok {
                healthy_ports += 1;
            } else {
                all_ok = false;
            }
            let icon = if ok {
                "✓".green().to_string()
            } else {
                "✗".red().to_string()
            };
            print!("  {} D:{}", icon, fwd.listen_port);
        }

        for fwd in &t.remote_forwards {
            print!("  {} R:{}", "—".dimmed(), fwd.bind_port);
        }
        println!();

        if !all_ok {
            println!(
                "  {}",
                "  ↳ some ports not reachable".yellow()
            );
        }
    }

    println!();
    if healthy_ports == total_ports {
        println!(
            "  {} All {} port(s) healthy across {} tunnel(s)",
            "✓".green(),
            total_ports,
            active.len()
        );
    } else {
        println!(
            "  {} {}/{} port(s) healthy across {} tunnel(s)",
            "✗".yellow(),
            healthy_ports,
            total_ports,
            active.len()
        );
    }

    Ok(())
}

fn cmd_edit(cfg: &Config) -> Result<()> {
    let editor = cfg.resolve_editor();

    let config_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?
        .join(".ssh")
        .join("config");

    let status = std::process::Command::new(&editor)
        .arg(&config_path)
        .status()
        .with_context(|| format!("failed to launch editor '{}'", editor))?;

    if !status.success() {
        anyhow::bail!("editor exited with {}", status);
    }

    Ok(())
}

fn cmd_logs(name: Option<String>, lines: usize, follow: bool) -> Result<()> {
    let tunnels = ssh_config::discover_tunnels()?;

    let tunnel_name = match name {
        Some(n) => {
            if !tunnels.iter().any(|t| t.name == n) {
                anyhow::bail!("tunnel '{}' not found in SSH config", n);
            }
            n
        }
        None => {
            let items: Vec<String> = tunnels.iter().map(|t| t.name.clone()).collect();
            if items.is_empty() {
                println!("{}", "No tunnels found.".yellow());
                return Ok(());
            }
            let idx = picker::pick("Show logs for", &items)?;
            tunnels[idx].name.clone()
        }
    };

    let log_path = process::log_file(&tunnel_name)?;

    if !log_path.exists() {
        println!(
            "{} No log file for '{}'",
            "⚠".yellow(),
            tunnel_name
        );
        return Ok(());
    }

    if log_path.metadata().map(|m| m.len()).unwrap_or(0) == 0 && !follow {
        println!("{} Log is empty — no errors from autossh", "✓".green());
        return Ok(());
    }

    let mut args = vec![format!("-n{}", lines)];
    if follow {
        args.push("-f".to_string());
    }
    args.push(log_path.to_string_lossy().to_string());

    let status = std::process::Command::new("tail")
        .args(&args)
        .status()
        .context("failed to run tail")?;

    if !status.success() {
        anyhow::bail!("tail exited with {}", status);
    }

    Ok(())
}

fn cmd_enable(name: Option<String>, group: Option<String>) -> Result<()> {
    let tunnels = ssh_config::discover_tunnels()?;

    if let Some(ref group) = group {
        let in_group = tunnels_in_group(&tunnels, group);
        if in_group.is_empty() {
            anyhow::bail!("no tunnels found in group '{}'", group);
        }

        let disabled: Vec<&&tunnel::TunnelHost> = in_group
            .iter()
            .filter(|t| !launchd::is_enabled(&t.name))
            .collect();

        if disabled.is_empty() {
            println!("{}", format!("All tunnels in group '{}' are already enabled.", group).yellow());
            return Ok(());
        }

        for t in &disabled {
            match launchd::enable(t) {
                Ok(()) => println!(
                    "{} {} auto-start {}",
                    "⏎".green(),
                    t.name.green().bold(),
                    "enabled".green()
                ),
                Err(e) => println!(
                    "{} {} — {}",
                    "✗".red(),
                    t.name.red().bold(),
                    e
                ),
            }
        }
        return Ok(());
    }

    let tunnel = match name {
        Some(ref n) => tunnels
            .iter()
            .find(|t| t.name == *n)
            .ok_or_else(|| anyhow::anyhow!("tunnel '{}' not found in SSH config", n))?,
        None => {
            let disabled: Vec<&tunnel::TunnelHost> = tunnels
                .iter()
                .filter(|t| !launchd::is_enabled(&t.name))
                .collect();

            if disabled.is_empty() {
                println!("{}", "All tunnels are already enabled.".yellow());
                return Ok(());
            }

            let items: Vec<String> = disabled
                .iter()
                .map(|t| format!("{} ({})", t.name, format_all_forwards(t)))
                .collect();

            let idx = picker::pick("Enable auto-start for", &items)?;
            disabled[idx]
        }
    };

    if launchd::is_enabled(&tunnel.name) {
        println!("{} is already enabled", tunnel.name.yellow());
        return Ok(());
    }

    launchd::enable(tunnel)?;
    println!(
        "{} {} auto-start {}",
        "⏎".green(),
        tunnel.name.green().bold(),
        "enabled".green()
    );

    Ok(())
}

fn cmd_disable(name: Option<String>, group: Option<String>) -> Result<()> {
    let tunnels = ssh_config::discover_tunnels()?;

    if let Some(ref group) = group {
        let in_group = tunnels_in_group(&tunnels, group);
        if in_group.is_empty() {
            anyhow::bail!("no tunnels found in group '{}'", group);
        }

        let enabled: Vec<&&tunnel::TunnelHost> = in_group
            .iter()
            .filter(|t| launchd::is_enabled(&t.name))
            .collect();

        if enabled.is_empty() {
            println!("{}", format!("No tunnels in group '{}' are enabled for auto-start.", group).yellow());
            return Ok(());
        }

        for t in &enabled {
            match launchd::disable(&t.name) {
                Ok(()) => println!(
                    "{} {} auto-start {}",
                    "○".dimmed(),
                    t.name.bold(),
                    "disabled".dimmed()
                ),
                Err(e) => println!(
                    "{} {} — {}",
                    "✗".red(),
                    t.name.red().bold(),
                    e
                ),
            }
        }
        return Ok(());
    }

    let tunnel_name = match name {
        Some(n) => {
            if !tunnels.iter().any(|t| t.name == n) {
                anyhow::bail!("tunnel '{}' not found in SSH config", n);
            }
            n
        }
        None => {
            let enabled: Vec<&tunnel::TunnelHost> = tunnels
                .iter()
                .filter(|t| launchd::is_enabled(&t.name))
                .collect();

            if enabled.is_empty() {
                println!("{}", "No tunnels are enabled for auto-start.".yellow());
                return Ok(());
            }

            let items: Vec<String> = enabled
                .iter()
                .map(|t| format!("{} ({})", t.name, format_all_forwards(t)))
                .collect();

            let idx = picker::pick("Disable auto-start for", &items)?;
            enabled[idx].name.clone()
        }
    };

    if !launchd::is_enabled(&tunnel_name) {
        println!("{} is not enabled", tunnel_name.yellow());
        return Ok(());
    }

    launchd::disable(&tunnel_name)?;
    println!(
        "{} {} auto-start {}",
        "○".dimmed(),
        tunnel_name.bold(),
        "disabled".dimmed()
    );

    Ok(())
}

fn cmd_config(cfg: &Config) -> Result<()> {
    let path = Config::init()?;
    let editor = cfg.resolve_editor();

    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("failed to launch editor '{}'", editor))?;

    if !status.success() {
        anyhow::bail!("editor exited with {}", status);
    }

    Ok(())
}

fn cmd_completions(shell: Option<clap_complete::Shell>, cfg: &Config) -> Result<()> {
    let shell = match shell {
        Some(s) => s,
        None => {
            let name = cfg.shell.as_deref()
                .ok_or_else(|| anyhow::anyhow!(
                    "no shell specified — use `mole completions <shell>` or set `shell` in ~/.mole/config.toml"
                ))?;
            name.parse::<clap_complete::Shell>()
                .map_err(|_| anyhow::anyhow!("unknown shell '{}' in config", name))?
        }
    };

    let shell_name = match shell {
        clap_complete::Shell::Bash => "bash",
        clap_complete::Shell::Zsh => "zsh",
        clap_complete::Shell::Fish => "fish",
        clap_complete::Shell::Elvish => "elvish",
        clap_complete::Shell::PowerShell => "powershell",
        _ => anyhow::bail!("unsupported shell"),
    };
    unsafe { std::env::set_var("COMPLETE", shell_name) };
    clap_complete::CompleteEnv::with_factory(Cli::command).complete();
    Ok(())
}

fn cmd_list_tunnel_names() -> Result<()> {
    let tunnels = ssh_config::discover_tunnels()?;
    for t in &tunnels {
        println!("{}", t.name);
    }
    Ok(())
}

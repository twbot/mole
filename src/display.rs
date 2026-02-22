use colored::Colorize;
use console::Alignment;

use crate::health;
use crate::launchd;
use crate::process;
use crate::tunnel::TunnelHost;

/// Print a formatted list of all tunnels with their status.
pub fn print_tunnel_list(tunnels: &[TunnelHost]) {
    if tunnels.is_empty() {
        println!("{}", "No tunnels found in ~/.ssh/config".yellow());
        println!("Add a Host block with LocalForward, RemoteForward, or DynamicForward to get started.");
        return;
    }

    // Pre-compute all row data
    let mut rows: Vec<Row> = Vec::new();
    for tunnel in tunnels {
        let active = process::is_active(&tunnel.name).unwrap_or(false);
        let enabled = launchd::is_enabled(&tunnel.name);
        let mut fwd_parts: Vec<String> = tunnel.forwards.iter().map(|f| f.to_string()).collect();
        fwd_parts.extend(tunnel.remote_forwards.iter().map(|f| f.to_string()));
        fwd_parts.extend(tunnel.dynamic_forwards.iter().map(|f| f.to_string()));
        let fwd_str = fwd_parts.join(", ");
        let has_local_forwards = !tunnel.forwards.is_empty() || !tunnel.dynamic_forwards.is_empty();

        if active {
            let pid = process::read_pid(&tunnel.name).ok().flatten();
            let uptime = process::get_start_time(&tunnel.name)
                .ok()
                .flatten()
                .map(process::format_uptime)
                .unwrap_or_default();
            let healthy = if has_local_forwards {
                let local_ok = tunnel.forwards.iter().all(|f| health::check_port(f.local_port));
                let dynamic_ok = tunnel.dynamic_forwards.iter().all(|f| health::check_port(f.listen_port));
                Some(local_ok && dynamic_ok)
            } else {
                None // remote-only tunnels can't be probed locally
            };

            rows.push(Row {
                name: tunnel.name.clone(),
                group: tunnel.group.clone(),
                active: true,
                status: format!("up {}", uptime),
                healthy,
                pid,
                fwd_str,
                enabled,
            });
        } else {
            rows.push(Row {
                name: tunnel.name.clone(),
                group: tunnel.group.clone(),
                active: false,
                status: "inactive".to_string(),
                healthy: None,
                pid: None,
                fwd_str,
                enabled,
            });
        }
    }

    // Column widths from plain text (name + optional group badge)
    let w_name = rows
        .iter()
        .map(|r| {
            let badge_len = r.group.as_ref().map_or(0, |g| g.len() + 3); // " [g]"
            r.name.len() + badge_len
        })
        .max()
        .unwrap_or(0);
    let w_status = rows.iter().map(|r| r.status.len()).max().unwrap_or(0);

    for row in &rows {
        let bullet = if row.active {
            "●".green().to_string()
        } else {
            "○".dimmed().to_string()
        };

        let name_with_badge = if let Some(ref g) = row.group {
            let badge = format!(" [{}]", g).dimmed().to_string();
            let name_colored = if row.active {
                row.name.green().bold().to_string()
            } else {
                row.name.to_string()
            };
            format!("{}{}", name_colored, badge)
        } else {
            if row.active {
                row.name.green().bold().to_string()
            } else {
                row.name.to_string()
            }
        };
        let name_pad = pad(&name_with_badge, w_name);

        let status_colored = if row.active {
            row.status.green().to_string()
        } else {
            row.status.dimmed().to_string()
        };
        let status_pad = pad(&status_colored, w_status);

        // Measure actual display width of health icons so the placeholder
        // matches even when ✓/✗ render as double-width in some fonts.
        let w_health = console::measure_text_width("✓").max(1);
        let health = match row.healthy {
            Some(true) => pad(&"✓".green().to_string(), w_health),
            Some(false) => pad(&"✗".red().to_string(), w_health),
            None => " ".repeat(w_health),
        };

        let fwd = row.fwd_str.dimmed().to_string();

        let mut suffix = String::new();
        if let Some(p) = row.pid {
            suffix.push_str(&format!("  {}", format!("pid {}", p).dimmed()));
        }
        if row.enabled {
            let icon = if row.active { "⏎".green().to_string() } else { "⏎".dimmed().to_string() };
            suffix.push_str(&format!("  {}", icon));
        }

        println!("  {} {}  {}  {}  {}{}", bullet, name_pad, status_pad, health, fwd, suffix);
    }
}

/// Pad an ANSI-colored string to a visible width using console's awareness of escape codes.
fn pad(s: &str, width: usize) -> String {
    console::pad_str(s, width, Alignment::Left, None).to_string()
}

struct Row {
    name: String,
    group: Option<String>,
    active: bool,
    status: String,
    healthy: Option<bool>,
    pid: Option<u32>,
    fwd_str: String,
    enabled: bool,
}

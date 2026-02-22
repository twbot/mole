use clap::{Parser, Subcommand};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};

#[derive(Parser)]
#[command(name = "mole", about = "SSH tunnel manager", version)]
pub struct Cli {
    /// Disable colored output
    #[arg(long, global = true)]
    pub no_color: bool,

    #[command(subcommand)]
    pub command: Command,
}

fn complete_tunnel_names(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_str().unwrap_or("");
    let tunnels = crate::ssh_config::discover_tunnels().unwrap_or_default();
    tunnels
        .iter()
        .filter(|t| t.name.starts_with(prefix))
        .map(|t| CompletionCandidate::new(&t.name))
        .collect()
}

fn complete_group_names(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_str().unwrap_or("");
    let tunnels = crate::ssh_config::discover_tunnels().unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    tunnels
        .iter()
        .filter_map(|t| t.group.as_deref())
        .filter(|g| g.starts_with(prefix))
        .filter(|g| seen.insert(g.to_string()))
        .map(|g| CompletionCandidate::new(g))
        .collect()
}

#[derive(Subcommand)]
pub enum Command {
    /// Start a tunnel
    Up {
        /// Tunnel name (interactive picker if omitted)
        #[arg(add = ArgValueCompleter::new(complete_tunnel_names))]
        name: Option<String>,
        /// Start all inactive tunnels
        #[arg(long, short, conflicts_with = "name")]
        all: bool,
        /// Start all inactive tunnels in a group
        #[arg(long, short, conflicts_with = "name", conflicts_with = "all", add = ArgValueCompleter::new(complete_group_names))]
        group: Option<String>,
        /// Auto-start this tunnel on login via launchd
        #[arg(long)]
        persist: bool,
    },
    /// Stop a tunnel
    Down {
        /// Tunnel name (interactive picker if omitted)
        #[arg(add = ArgValueCompleter::new(complete_tunnel_names))]
        name: Option<String>,
        /// Stop all active tunnels
        #[arg(long, short, conflicts_with = "name")]
        all: bool,
        /// Stop all active tunnels in a group
        #[arg(long, short, conflicts_with = "name", conflicts_with = "all", add = ArgValueCompleter::new(complete_group_names))]
        group: Option<String>,
    },
    /// Remove a tunnel from SSH config
    Remove {
        /// Tunnel name (interactive picker if omitted)
        #[arg(add = ArgValueCompleter::new(complete_tunnel_names))]
        name: Option<String>,
    },
    /// Restart a tunnel (down + up)
    Restart {
        /// Tunnel name (interactive picker if omitted)
        #[arg(add = ArgValueCompleter::new(complete_tunnel_names))]
        name: Option<String>,
        /// Restart all active tunnels
        #[arg(long, short, conflicts_with = "name")]
        all: bool,
        /// Restart all tunnels in a group
        #[arg(long, short, conflicts_with = "name", conflicts_with = "all", add = ArgValueCompleter::new(complete_group_names))]
        group: Option<String>,
    },
    /// List all tunnels and their status
    #[command(alias = "ls", alias = "status")]
    List {
        /// Filter tunnels by group
        #[arg(long, short, add = ArgValueCompleter::new(complete_group_names))]
        group: Option<String>,
    },
    /// Health-check all active tunnels
    Check,
    /// Add a new tunnel interactively
    Add,
    /// Open ~/.ssh/config in your editor
    Edit,
    /// Show tunnel logs
    Logs {
        /// Tunnel name (interactive picker if omitted)
        #[arg(add = ArgValueCompleter::new(complete_tunnel_names))]
        name: Option<String>,
        /// Number of lines to show
        #[arg(short = 'n', long, default_value = "50")]
        lines: usize,
        /// Follow log output
        #[arg(short, long)]
        follow: bool,
    },
    /// Enable auto-start on login via launchd
    Enable {
        /// Tunnel name (interactive picker if omitted)
        #[arg(add = ArgValueCompleter::new(complete_tunnel_names))]
        name: Option<String>,
        /// Enable all tunnels in a group
        #[arg(long, short, conflicts_with = "name", add = ArgValueCompleter::new(complete_group_names))]
        group: Option<String>,
    },
    /// Disable auto-start on login
    Disable {
        /// Tunnel name (interactive picker if omitted)
        #[arg(add = ArgValueCompleter::new(complete_tunnel_names))]
        name: Option<String>,
        /// Disable all tunnels in a group
        #[arg(long, short, conflicts_with = "name", add = ArgValueCompleter::new(complete_group_names))]
        group: Option<String>,
    },
    /// Rename a tunnel
    Rename {
        /// Current tunnel name (interactive picker if omitted)
        #[arg(add = ArgValueCompleter::new(complete_tunnel_names))]
        old: Option<String>,
        /// New tunnel name
        new_name: String,
    },
    /// Initialize or edit ~/.mole/config.toml
    Config,
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for (reads from config if omitted)
        shell: Option<clap_complete::Shell>,
    },
    /// List tunnel names (for shell completion scripts)
    #[command(hide = true)]
    ListTunnelNames,
}

/// A single port forward: local_port -> remote_host:remote_port
#[derive(Debug, Clone)]
pub struct PortForward {
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

impl std::fmt::Display for PortForward {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.local_port, self.remote_host, self.remote_port)
    }
}

/// A single remote (reverse) forward: remote bind_port -> local remote_host:remote_port
#[derive(Debug, Clone)]
pub struct RemotePortForward {
    pub bind_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

impl std::fmt::Display for RemotePortForward {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "R:{}→{}:{}", self.bind_port, self.remote_host, self.remote_port)
    }
}

/// A dynamic (SOCKS proxy) forward: ssh -D listen_port
#[derive(Debug, Clone)]
pub struct DynamicForward {
    pub listen_port: u16,
}

impl std::fmt::Display for DynamicForward {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "D:{}", self.listen_port)
    }
}

/// An SSH host that has at least one forward (local, remote, or dynamic) — i.e., a tunnel.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TunnelHost {
    pub name: String,
    pub hostname: Option<String>,
    pub forwards: Vec<PortForward>,
    pub remote_forwards: Vec<RemotePortForward>,
    pub dynamic_forwards: Vec<DynamicForward>,
    pub group: Option<String>,
}

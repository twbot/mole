use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// Check if a local port is accepting connections (tunnel is healthy).
pub fn check_port(port: u16) -> bool {
    let addr = format!("127.0.0.1:{}", port);
    TcpStream::connect_timeout(
        &addr.parse().unwrap(),
        Duration::from_secs(2),
    )
    .is_ok()
}

/// Check if a local port is free (not already bound by another process).
pub fn is_port_free(port: u16) -> bool {
    TcpListener::bind(format!("127.0.0.1:{}", port)).is_ok()
}

/// Probe a list of local ports with retries over a timeout period.
/// Returns true if all ports became reachable within the timeout.
pub fn wait_healthy_ports(ports: &[u16], timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        if ports.iter().all(|&p| check_port(p)) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

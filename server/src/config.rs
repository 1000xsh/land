use std::net::SocketAddr;

/// configuration for the RPC server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// address to bind the TCP server to.
    pub bind_addr: SocketAddr,

    /// size of the request queue (SPSC ring buffer).
    /// must be a power of 2.
    pub queue_size: usize,

    /// CPU core to pin the worker thread to.
    pub worker_cpu_core: Option<usize>,

    /// maximum size of HTTP request body in bytes.
    pub max_request_size: usize,

    /// maximum number of concurrent TCP connections.
    pub max_connections: usize,

    /// TCP receive buffer size.
    pub tcp_recv_buffer_size: usize,

    /// TCP send buffer size.
    pub tcp_send_buffer_size: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:8080".parse().unwrap(),
            queue_size: 4096, // power of 2
            worker_cpu_core: Some(5),
            max_request_size: 1024 * 1024, // 1MB
            max_connections: 10240,
            tcp_recv_buffer_size: 64 * 1024, // 64KB
            tcp_send_buffer_size: 64 * 1024, // 64KB
        }
    }
}

impl ServerConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// set bind address.
    pub fn with_bind_addr(mut self, addr: SocketAddr) -> Self {
        self.bind_addr = addr;
        self
    }

    /// set queue size. must be a power of 2.
    pub fn with_queue_size(mut self, size: usize) -> Self {
        assert!(
            size.is_power_of_two(),
            "Queue size must be a power of 2, got {}",
            size
        );
        self.queue_size = size;
        self
    }

    /// set worker CPU core for affinity.
    pub fn with_worker_cpu_core(mut self, core: usize) -> Self {
        self.worker_cpu_core = Some(core);
        self
    }

    /// set maximum request size.
    pub fn with_max_request_size(mut self, size: usize) -> Self {
        self.max_request_size = size;
        self
    }

    /// set maximum number of concurrent connections.
    pub fn with_max_connections(mut self, count: usize) -> Self {
        self.max_connections = count;
        self
    }

    /// set TCP buffer sizes.
    pub fn with_tcp_buffer_sizes(mut self, recv: usize, send: usize) -> Self {
        self.tcp_recv_buffer_size = recv;
        self.tcp_send_buffer_size = send;
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.queue_size.is_power_of_two() {
            return Err(format!(
                "Queue size must be a power of 2, got {}",
                self.queue_size
            ));
        }

        if self.max_request_size == 0 {
            return Err("Max request size must be greater than 0".to_string());
        }

        if self.max_connections == 0 {
            return Err("Max connections must be greater than 0".to_string());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ServerConfig::default();
        assert!(config.validate().is_ok());
        assert_eq!(config.bind_addr.port(), 8080);
        assert!(config.queue_size.is_power_of_two());
    }

    #[test]
    fn test_builder_pattern() {
        let config = ServerConfig::new()
            .with_bind_addr("127.0.0.1:9000".parse().unwrap())
            .with_queue_size(8192)
            .with_worker_cpu_core(2);

        assert_eq!(config.bind_addr.port(), 9000);
        assert_eq!(config.queue_size, 8192);
        assert_eq!(config.worker_cpu_core, Some(2));
    }

    #[test]
    #[should_panic(expected = "Queue size must be a power of 2")]
    fn test_invalid_queue_size() {
        ServerConfig::new().with_queue_size(1000);
    }
}

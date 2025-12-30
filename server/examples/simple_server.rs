use land_server::{RpcServer, ServerConfig};

fn main() {
    // init
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Debug)
        .init();

    // create server configuration
    let config = ServerConfig::new()
        .with_bind_addr("127.0.0.1:8080".parse().unwrap())
        .with_queue_size(4096)
        // pin worker to cpu
        .with_worker_cpu_core(5)
        .with_max_connections(1024);

    println!("Starting RPC server...");
    println!("Configuration:");
    println!("  Bind address: {}", config.bind_addr);
    println!("  Queue size: {}", config.queue_size);
    println!("  Worker CPU core: {:?}", config.worker_cpu_core);
    println!("  Max connections: {}", config.max_connections);
    println!();
    println!("Test with curl:");
    println!(
        r#"  curl -X POST http://127.0.0.1:8080/ \
    -H 'Content-Type: application/json' \
    -d '{{
      "jsonrpc": "2.0",
      "id": 1,
      "method": "sendTransaction",
      "params": {{
        "transaction": "SGVsbG8gV29ybGQh",
        "fanout": 3,
        "neighbor_fanout": 3
      }}
    }}'
"#
    );

    // create and run server
    let server = RpcServer::new(config).expect("Failed to create server");

    // run server (blocking)
    // press ctrl+c to stop the server
    if let Err(e) = server.run() {
        eprintln!("Server error: {}", e);
        std::process::exit(1);
    }

    println!("Server stopped");
}

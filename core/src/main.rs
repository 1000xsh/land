//! land-core: ultra low-latency transaction sender.
//!
//! thin orchestration: init -> wire -> run

use land_core::config::{Config, CoreAllocation};
use land_core::warmup::{warmup, WarmupConfig};
use land_leader::{Config as LeaderConfig, LeaderTracker};
use land_quic::{SenderConfig, TransactionSender};
use land_server::{RpcServer, ServerConfig};
use std::env;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // debug builds: init logger with nanosecond timestamps
    #[cfg(debug_assertions)]
    {
        use std::io::Write;
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug"))
            .format(|buf, record| {
                let ts = buf.timestamp_nanos();
                writeln!(
                    buf,
                    "[{} {:5} {}:{}] {}",
                    ts,
                    record.level(),
                    record.module_path().unwrap_or(""),
                    record.line().unwrap_or(0),
                    record.args()
                )
            })
            .init();
    }

    let config = parse_config();

    eprintln!("[core] starting");
    eprintln!("[core] rpc={}", config.leader.rpc_url);
    eprintln!("[core] ws={}", config.leader.ws_url);
    eprintln!("[core] http={}", config.http.bind_addr);
    eprintln!("[core] staked={:?}", config.quic.staked_keypair);
    eprintln!(
        "[core] cores: leader={:?} sender={:?} (adaptive JIT)",
        config.allocation.leader_tracker, config.allocation.quic_sender,
    );

    // shared shutdown signal
    let shutdown = Arc::new(AtomicBool::new(false));

    // start leader tracker
    eprintln!("[core] starting leader tracker...");
    let mut leader_config = LeaderConfig::custom(&config.leader.rpc_url, &config.leader.ws_url)
        .with_refresh_interval(config.leader.schedule_refresh_interval);
    if let Some(core) = config.allocation.leader_tracker {
        leader_config = leader_config.with_cpu_core(core);
    }

    let mut leader_tracker = LeaderTracker::new(leader_config);
    if let Err(e) = leader_tracker.start().await {
        eprintln!("[core] leader tracker failed: {}", e);
        std::process::exit(1);
    }
    eprintln!("[core] leader tracker started");

    // get leader buffer (implements LeaderLookup)
    let leader_buffer = leader_tracker.buffer();

    // warmup caches
    eprintln!("[core] warmup...");
    warmup(&leader_buffer, &WarmupConfig::default());

    // create transaction sender with leader buffer
    eprintln!("[core] creating quic sender...");
    let mut quic_config = land_quic::Config::default();
    quic_config.idle_timeout_ms = config.quic.idle_timeout.as_millis() as u64;
    quic_config.connect_timeout_ms = config.quic.connect_timeout.as_millis() as u64;
    if let Some(ref keypair_path) = config.quic.staked_keypair {
        quic_config = quic_config.with_staked_identity(keypair_path.to_str().unwrap());
    }

    // adaptive jit strategy - connects on-demand with timing awareness
    let adaptive_config = land_quic::AdaptivePrewarmConfig {
        min_solana_slots: 2,          // minimum 2 solana slots early (800ms)
        max_solana_slots: 10,         // maximum 10 Solana slots early (4s)
        solana_slot_duration_ms: 400, // solana slot duration
        default_handshake_ms: 2000,   // default handshake time for unknown validators (2s)
        bootstrap_from_median: true,  // use median from known validators for bootstrap
        timing_percentile: 90.0,      // use p90 for conservative estimation
    };

    let strategy = land_quic::ConnectStrategy::adaptive_jit(
        land_quic::JitMode::Single, // connect only to current leader
        adaptive_config,            // enable adaptive timing checks
    );

    // InlinePrewarm with adaptive timing
    // let strategy = land_quic::ConnectStrategy::InlinePrewarm(land_quic::InlinePrewarmConfig {
    //     lookahead: 2,                            // prewarm connections to next 2 leaders
    //     cpu_core: config.allocation.quic_sender, // pin hot loop to sender core
    //     adaptive: Some(adaptive_config),         // enable adaptive timing
    // });

    let sender_config = SenderConfig {
        strategy,
        quic_config,
        ..SenderConfig::default()
    };
    let sender =
        TransactionSender::new(sender_config, leader_buffer.clone(), config.quic.bind_addr)
            .expect("failed to create sender");
    eprintln!("[core] quic sender ready (ADAPTIVE JIT - on-demand with timing awareness)");

    // create and start rpc server
    eprintln!("[core] starting rpc server...");
    let mut server_config = ServerConfig::new()
        .with_bind_addr(config.http.bind_addr)
        .with_queue_size(config.http.queue_size)
        .with_max_connections(config.http.max_connections)
        .with_max_request_size(config.http.max_request_size);
    if let Some(core) = config.allocation.quic_sender {
        server_config = server_config.with_worker_cpu_core(core);
    }

    let server = RpcServer::new(server_config).expect("failed to create server");

    // run server with sender in background thread
    std::thread::spawn(move || {
        if let Err(e) = server.run_with_sender(sender) {
            eprintln!("[http] error: {}", e);
        }
    });

    eprintln!(
        "[core] ready | slot={}",
        leader_tracker.buffer().current_slot()
    );

    // periodic slot logging
    let buffer_clone = leader_tracker.buffer();
    let shutdown_clone = Arc::clone(&shutdown);
    std::thread::spawn(move || {
        let mut last_slot = 0;
        loop {
            // check shutdown signal
            if shutdown_clone.load(Ordering::Acquire) {
                break;
            }

            let slot = buffer_clone.current_slot();
            if slot != last_slot {
                eprintln!("[core] slot={}", slot);
                last_slot = slot;
            }
        }
    });

    // wait for ctrl+c
    tokio::signal::ctrl_c().await.ok();

    eprintln!("[core] shutting down...");
    shutdown.store(true, Ordering::Release);
    leader_tracker.stop().await;
    eprintln!("[core] done");
}

fn parse_config() -> Config {
    let args: Vec<String> = env::args().collect();

    let rpc_url = arg_value(&args, "--rpc")
        .or_else(|| env::var("RPC_URL").ok())
        .unwrap_or_else(|| "http://45.152.160.253:8899".into());

    let ws_url = arg_value(&args, "--ws")
        .or_else(|| env::var("WS_URL").ok())
        .unwrap_or_else(|| "ws://45.152.160.253:8900".into());

    let http_addr: SocketAddr = arg_value(&args, "--http")
        .or_else(|| env::var("HTTP_ADDR").ok())
        .unwrap_or_else(|| "127.0.0.1:8080".into())
        .parse()
        .expect("invalid http address");

    let keypair_path = arg_value(&args, "--keypair").or_else(|| env::var("KEYPAIR_PATH").ok());

    let mut config = Config::custom(rpc_url, ws_url)
        .with_http_addr(http_addr)
        .with_allocation(CoreAllocation::auto_detect());

    if let Some(path) = keypair_path {
        config = config.with_staked_keypair(path);
    }

    config
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

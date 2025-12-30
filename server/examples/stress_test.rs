use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_HOST: &str = "127.0.0.1:8080";
const DEFAULT_CONCURRENCY: usize = 10;
const DEFAULT_REQUESTS: usize = 10000;

struct Stats {
    latencies: Vec<u64>, // latencies in microseconds
    errors: u64,
    total_duration: Duration,
}

impl Stats {
    fn calculate(&mut self) {
        // sort latencies for percentiles
        self.latencies.sort_unstable();
    }

    fn print(&self) {
        let total_requests = self.latencies.len() + self.errors as usize;
        let successful = self.latencies.len();
        let throughput = successful as f64 / self.total_duration.as_secs_f64();

        println!("\n========== Results ==========");
        println!("Total requests:    {}", total_requests);
        println!("Successful:        {}", successful);
        println!("Failed:            {}", self.errors);
        println!("Duration:          {:.2}s", self.total_duration.as_secs_f64());
        println!("Throughput:        {:.2} req/s", throughput);

        if !self.latencies.is_empty() {
            let min = self.latencies[0];
            let max = self.latencies[self.latencies.len() - 1];
            let avg: u64 = self.latencies.iter().sum::<u64>() / self.latencies.len() as u64;
            let p50 = self.latencies[self.latencies.len() / 2];
            let p95 = self.latencies[self.latencies.len() * 95 / 100];
            let p99 = self.latencies[self.latencies.len() * 99 / 100];

            println!("\nLatency (μs):");
            println!("  Min:             {}", min);
            println!("  Avg:             {}", avg);
            println!("  p50:             {}", p50);
            println!("  p95:             {}", p95);
            println!("  p99:             {}", p99);
            println!("  Max:             {}", max);
        }
        println!("=============================\n");
    }
}

fn send_request(host: &str, request_id: u64) -> Result<u64, String> {
    let start = Instant::now();

    // connect
    let mut stream = TcpStream::connect(host)
        .map_err(|e| format!("Connection failed: {}", e))?;

    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .ok();

    // build JSON-RPC request
    let json_body = format!(
        r#"{{"jsonrpc":"2.0","id":{},"method":"sendTransaction","params":{{"transaction":"SGVsbG8gV29ybGQh","fanout":3,"target_slot":12345678}}}}"#,
        request_id
    );

    // build HTTP request
    let http_request = format!(
        "POST / HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        host,
        json_body.len(),
        json_body
    );

    // send request
    stream
        .write_all(http_request.as_bytes())
        .map_err(|e| format!("Write failed: {}", e))?;

    stream
        .flush()
        .map_err(|e| format!("Flush failed: {}", e))?;

    // read response
    let mut buffer = vec![0u8; 4096];
    let n = stream
        .read(&mut buffer)
        .map_err(|e| format!("Read failed: {}", e))?;

    if n == 0 {
        return Err("Empty response".to_string());
    }

    // parse response to check for success
    let response = String::from_utf8_lossy(&buffer[..n]);
    if !response.contains("HTTP/1.1 200 OK") {
        return Err(format!("Non-200 response: {}", &response[..200.min(n)]));
    }

    let elapsed = start.elapsed().as_micros() as u64;
    Ok(elapsed)
}

fn worker_thread(
    host: String,
    start_id: u64,
    count: usize,
    completed: Arc<AtomicU64>,
) -> Vec<Result<u64, String>> {
    let mut results = Vec::with_capacity(count);

    for i in 0..count {
        let request_id = start_id + i as u64;
        let result = send_request(&host, request_id);
        results.push(result);

        completed.fetch_add(1, Ordering::Relaxed);
    }

    results
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // parse CLI arguments
    let host = args
        .iter()
        .position(|a| a == "--host" || a == "-h")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or(DEFAULT_HOST);

    let concurrency: usize = args
        .iter()
        .position(|a| a == "--concurrency" || a == "-c")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_CONCURRENCY);

    let total_requests: usize = args
        .iter()
        .position(|a| a == "--requests" || a == "-n")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_REQUESTS);

    println!("========== Stress Test ==========");
    println!("Target:            {}", host);
    println!("Concurrency:       {}", concurrency);
    println!("Total requests:    {}", total_requests);
    println!("=================================\n");

    // calculate requests per thread
    let requests_per_thread = total_requests / concurrency;
    let remainder = total_requests % concurrency;

    let completed = Arc::new(AtomicU64::new(0));
    let completed_clone = completed.clone();

    // progress reporter thread
    let progress_handle = thread::spawn(move || {
        let start = Instant::now();
        loop {
            thread::sleep(Duration::from_secs(1));
            let count = completed_clone.load(Ordering::Relaxed);
            let elapsed = start.elapsed().as_secs_f64();
            let rate = count as f64 / elapsed;
            print!(
                "\rProgress: {}/{} ({:.0} req/s)    ",
                count, total_requests, rate
            );
            std::io::stdout().flush().unwrap();

            if count >= total_requests as u64 {
                break;
            }
        }
        println!();
    });

    // spawn worker threads
    let start_time = Instant::now();
    let mut handles = Vec::new();

    for i in 0..concurrency {
        let host = host.to_string();
        let start_id = (i * requests_per_thread) as u64;
        let count = if i == concurrency - 1 {
            requests_per_thread + remainder
        } else {
            requests_per_thread
        };
        let completed = completed.clone();

        let handle = thread::spawn(move || worker_thread(host, start_id, count, completed));
        handles.push(handle);
    }

    // collect results
    let mut all_latencies = Vec::new();
    let mut error_count = 0u64;

    for handle in handles {
        let results = handle.join().unwrap();
        for result in results {
            match result {
                Ok(latency) => all_latencies.push(latency),
                Err(_) => error_count += 1,
            }
        }
    }

    let total_duration = start_time.elapsed();

    // wait for progress reporter
    progress_handle.join().unwrap();

    // calculate and print stats
    let mut stats = Stats {
        latencies: all_latencies,
        errors: error_count,
        total_duration,
    };

    stats.calculate();
    stats.print();
}

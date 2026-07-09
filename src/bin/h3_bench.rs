use std::error::Error;
use std::future::poll_fn;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Buf;
use h3::client;
use http::Request;
use quinn::crypto::rustls::QuicClientConfig;
use rustls::RootCertStore;

const WORKERS: usize = 50;
const REPS: usize = 20;
const COUNTRY: &str = "CZ";
const LIMIT: usize = 10;
const SERVER_NAME: &str = "localhost";
const H3_CERT_PATH: &str = "/tmp/addresswise-h3-cert.der";
const QUERIES: [&str; 15] = [
    "N",
    "Na",
    "Na ",
    "Na p",
    "Na pa",
    "Na pas",
    "Na pase",
    "Na pasek",
    "Na paseka",
    "Na pasekach",
    "Na pasekach ",
    "Na pasekach 3",
    "Na pasekach 30",
    "Na pasekach 3085",
    "Na pasekach 3085/20",
];

type AppResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Clone, Copy)]
struct QueryStats {
    total_ms: f64,
    min_ms: f64,
    max_ms: f64,
}

impl Default for QueryStats {
    fn default() -> Self {
        Self {
            total_ms: 0.0,
            min_ms: f64::MAX,
            max_ms: 0.0,
        }
    }
}

#[tokio::main]
async fn main() -> AppResult<()> {
    let endpoint = client_endpoint()?;
    let start = Instant::now();
    let mut all_runs = Vec::with_capacity(WORKERS * REPS);

    let mut tasks = Vec::with_capacity(WORKERS);
    for worker in 0..WORKERS {
        let endpoint = endpoint.clone();
        tasks.push(tokio::spawn(async move {
            run_worker(worker, endpoint).await
        }));
    }

    for task in tasks {
        let worker_runs = task.await??;
        all_runs.extend(worker_runs);
    }

    print_report(&all_runs, start.elapsed());
    Ok(())
}

async fn run_worker(worker: usize, endpoint: quinn::Endpoint) -> AppResult<Vec<Vec<f64>>> {
    let connection = endpoint.connect(target_addr(), SERVER_NAME)?.await?;
    let (mut driver, mut sender) = client::new(h3_quinn::Connection::new(connection)).await?;
    let driver_task = tokio::spawn(async move { poll_fn(|cx| driver.poll_close(cx)).await });
    let mut runs = Vec::with_capacity(REPS);

    for rep in 0..REPS {
        let mut timings = Vec::with_capacity(QUERIES.len());
        for query in QUERIES {
            let elapsed = timed_query(&mut sender, query).await?;
            timings.push(elapsed);
        }

        let total_ms = timings.iter().sum::<f64>();
        println!("worker={worker:02} rep={rep:02} total={total_ms:.3} ms");
        runs.push(timings);
    }

    drop(sender);
    let _ = driver_task.await?;

    Ok(runs)
}

async fn timed_query(
    sender: &mut h3::client::SendRequest<h3_quinn::OpenStreams, bytes::Bytes>,
    query: &str,
) -> AppResult<f64> {
    let started = Instant::now();
    run_query(sender, query).await?;
    Ok(started.elapsed().as_secs_f64() * 1000.0)
}

async fn run_query(
    sender: &mut h3::client::SendRequest<h3_quinn::OpenStreams, bytes::Bytes>,
    query: &str,
) -> AppResult<()> {
    let request = Request::get(request_uri(query)).body(())?;
    let mut request_stream = sender.send_request(request).await?;
    let _response = request_stream.recv_response().await?;

    while let Some(mut chunk) = request_stream.recv_data().await? {
        while chunk.has_remaining() {
            let _ = chunk.copy_to_bytes(chunk.remaining());
        }
    }

    Ok(())
}

fn request_uri(query: &str) -> String {
    let encoded = query
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                [Some(byte as char), None, None]
            }
            b' ' => [Some('%'), Some('2'), Some('0')],
            _ => {
                let hex = format!("%{:02X}", byte);
                let mut chars = hex.chars();
                [chars.next(), chars.next(), chars.next()]
            }
        })
        .flatten()
        .collect::<String>();

    format!(
        "https://{SERVER_NAME}:{}/search?q={encoded}&country={COUNTRY}&limit={LIMIT}",
        target_addr().port()
    )
}

fn client_endpoint() -> AppResult<quinn::Endpoint> {
    let cert_der = fs::read(H3_CERT_PATH)?;

    let mut roots = RootCertStore::empty();
    roots.add(cert_der.into())?;

    let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_root_certificates(roots)
    .with_no_client_auth();
    crypto.alpn_protocols = vec![b"h3".to_vec()];
    crypto.enable_early_data = true;

    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(Duration::from_secs(10))
            .map_err(|error| format!("invalid idle timeout: {error}"))?,
    ));

    let mut client_config = quinn::ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(crypto)?,
    ));
    client_config.transport_config(Arc::new(transport));

    let mut endpoint = quinn::Endpoint::client("[::]:0".parse()?)?;
    endpoint.set_default_client_config(client_config);

    Ok(endpoint)
}

fn target_addr() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 8080))
}

fn print_report(all_runs: &[Vec<f64>], wall: Duration) {
    let mut query_stats = [QueryStats::default(); QUERIES.len()];
    let mut run_stats = QueryStats::default();

    for timings in all_runs {
        let total_ms = timings.iter().sum::<f64>();
        accumulate(&mut run_stats, total_ms);

        for (idx, timing) in timings.iter().copied().enumerate() {
            accumulate(&mut query_stats[idx], timing);
        }
    }

    let run_count = all_runs.len() as f64;

    println!();
    println!("HTTP/3 benchmark");
    println!("workers={WORKERS} reps={REPS} runs={} wall={:.3} ms", all_runs.len(), wall.as_secs_f64() * 1000.0);
    println!();
    println!("Per query:");
    for (idx, query) in QUERIES.iter().enumerate() {
        let stats = query_stats[idx];
        println!(
            "{idx:02}. {:<20} avg={:.3} ms min={:.3} ms max={:.3} ms",
            query,
            stats.total_ms / run_count,
            stats.min_ms,
            stats.max_ms
        );
    }
    println!();
    println!(
        "Total avg={:.3} ms min={:.3} ms max={:.3} ms",
        run_stats.total_ms / run_count,
        run_stats.min_ms,
        run_stats.max_ms
    );
}

fn accumulate(stats: &mut QueryStats, sample_ms: f64) {
    stats.total_ms += sample_ms;
    stats.min_ms = stats.min_ms.min(sample_ms);
    stats.max_ms = stats.max_ms.max(sample_ms);
}

mod http;
mod indexing;
pub mod models;
mod normalize;
mod search;

use std::env;
use std::error::Error;
use std::path::PathBuf;
use std::time::Instant;

use http::serve;
use indexing::{build_indices_from_postgres, build_indices_to_dir, load_indices_from_dir};

pub type AppResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[tokio::main]
async fn main() -> AppResult<()> {
    let command = Command::from_env()?;
    let country_codes = country_codes_from_env();
    match command {
        Command::BuildIndexes { index_dir } => build_indexes_command(&country_codes, index_dir),
        Command::Serve {
            host,
            port,
            index_dir,
        } => serve_command(&country_codes, host, port, index_dir),
        Command::Dev { host, port } => dev_command(&country_codes, host, port),
    }
}

fn country_codes_from_env() -> Vec<String> {
    let raw = env::var("COUNTRY_CODES")
        .or_else(|_| env::var("COUNTRY_CODE"))
        .unwrap_or_else(|_| String::from("SK"));
    let country_codes = raw
        .split(',')
        .map(|country_code| country_code.trim().to_uppercase())
        .filter(|country_code| !country_code.is_empty())
        .collect::<Vec<_>>();

    if country_codes.is_empty() {
        vec![String::from("SK")]
    } else {
        country_codes
    }
}

enum Command {
    BuildIndexes { index_dir: PathBuf },
    Serve { host: String, port: u16, index_dir: PathBuf },
    Dev { host: String, port: u16 },
}

impl Command {
    fn from_env() -> AppResult<Self> {
        let mut args = env::args().skip(1);
        match args.next().as_deref() {
            Some("build-indexes") => Ok(Self::BuildIndexes {
                index_dir: index_dir_from_env(),
            }),
            Some("serve") => Ok(Self::Serve {
                host: host_from_env(),
                port: port_from_env()?,
                index_dir: index_dir_from_env(),
            }),
            Some("dev") => Ok(Self::Dev {
                host: host_from_env(),
                port: port_from_env()?,
            }),
            Some(other) => Err(format!(
                "unknown command `{other}`. Use `build-indexes`, `serve`, or `dev`."
            )
            .into()),
            None => Ok(Self::Serve {
                host: host_from_env(),
                port: port_from_env()?,
                index_dir: index_dir_from_env(),
            }),
        }
    }
}

fn build_indexes_command(country_codes: &[String], index_dir: PathBuf) -> AppResult<()> {
    let started = Instant::now();
    let indexed_counts = build_indices_to_dir(country_codes, &index_dir)?;

    println!(
        "Built {} country index(es) into {} in {:.2?}.",
        country_codes.len(),
        index_dir.display(),
        started.elapsed()
    );
    for (country_code, indexed_count) in indexed_counts {
        println!("{country_code}: {indexed_count} indexed addresses");
    }

    Ok(())
}

fn serve_command(
    country_codes: &[String],
    host: String,
    port: u16,
    index_dir: PathBuf,
) -> AppResult<()> {
    let started = Instant::now();
    let address_indexes = load_indices_from_dir(country_codes, &index_dir)?;

    println!(
        "Loaded {} country index(es) from {} in {:.2?}.",
        country_codes.len(),
        index_dir.display(),
        started.elapsed()
    );
    print_country_counts(&address_indexes);
    print_endpoints(&host, port);

    serve(format!("{host}:{port}"), std::sync::Arc::new(address_indexes))
}

fn dev_command(country_codes: &[String], host: String, port: u16) -> AppResult<()> {
    let started = Instant::now();
    let (address_indexes, indexed_counts) = build_indices_from_postgres(country_codes)?;

    for (country_code, indexed_count) in indexed_counts {
        println!("Indexed {indexed_count} active {country_code} addresses.");
    }
    println!(
        "Built {} country index(es) in {:.2?}.",
        country_codes.len(),
        started.elapsed()
    );
    print_endpoints(&host, port);

    serve(format!("{host}:{port}"), std::sync::Arc::new(address_indexes))
}

fn print_country_counts(indexes: &search::AddressIndexes) {
    let mut country_codes = indexes.country_codes();
    country_codes.sort_unstable();

    for country_code in country_codes {
        if let Some(index) = indexes.by_country.get(country_code) {
            println!("{country_code}: {} indexed addresses", index.doc_count());
        }
    }
}

fn print_endpoints(host: &str, port: u16) {
    println!("Autocomplete endpoint: http://{host}:{port}/search?q=ba&country=SK");
    println!("Try: curl 'http://{host}:{port}/search?q=banska%2015&country=SK'");
}

fn host_from_env() -> String {
    env::var("HOST").unwrap_or_else(|_| String::from("127.0.0.1"))
}

fn port_from_env() -> AppResult<u16> {
    env::var("PORT")
        .unwrap_or_else(|_| String::from("8080"))
        .parse()
        .map_err(|error| format!("invalid PORT: {error}").into())
}

fn index_dir_from_env() -> PathBuf {
    env::var("INDEX_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./data/indexes"))
}

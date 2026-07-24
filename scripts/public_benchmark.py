#!/usr/bin/env python3

import argparse
import concurrent.futures
from datetime import UTC, datetime
import json
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path
from urllib.parse import quote, urlsplit


DEFAULT_DB_URL = "postgres://address:address@127.0.0.1:5432/address_wise"
DEFAULT_CURL_BIN = str(Path.home() / ".local/curl-http3/bin/curl")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Benchmark addresswise.eu by simulating per-character address entry "
            "with parallel workers."
        )
    )
    parser.add_argument(
        "--base-url",
        default="https://addresswise.eu",
        help="Public endpoint base URL.",
    )
    parser.add_argument(
        "--protocol",
        choices=("http2", "http3", "both"),
        default="both",
        help="Protocol benchmark mode.",
    )
    parser.add_argument(
        "--workers",
        type=int,
        default=100,
        help="Parallel worker count.",
    )
    parser.add_argument(
        "--sample-size",
        type=int,
        default=100,
        help="Number of random addresses to sample from PostgreSQL.",
    )
    parser.add_argument(
        "--sample-percent",
        type=float,
        default=0.5,
        help="TABLESAMPLE SYSTEM percentage used for the address pool query.",
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=10,
        help="Search endpoint limit parameter.",
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=30,
        help="Per curl process timeout in seconds.",
    )
    parser.add_argument(
        "--db-url",
        default=DEFAULT_DB_URL,
        help="PostgreSQL DSN used for random sampling when --queries-file is not provided.",
    )
    parser.add_argument(
        "--queries-file",
        help=(
            "Optional TSV file with one 'COUNTRY<TAB>QUERY' per line. "
            "When set, skips DB sampling entirely."
        ),
    )
    parser.add_argument(
        "--curl-bin",
        default=DEFAULT_CURL_BIN,
        help="Curl binary with HTTP/2 and HTTP/3 support.",
    )
    parser.add_argument(
        "--api-key",
        default=os.environ.get("ADDRESSWISE_BENCHMARK_API_KEY"),
        help=(
            "API key for authenticated search requests. Defaults to the "
            "ADDRESSWISE_BENCHMARK_API_KEY environment variable."
        ),
    )
    parser.add_argument(
        "--origin",
        help=(
            "Origin header for authenticated requests. Defaults to the origin "
            "of --base-url when --api-key is set."
        ),
    )
    parser.add_argument(
        "--street-only",
        action="store_true",
        help="Add the bare street_only flag to each search request.",
    )
    parser.add_argument(
        "--all-countries",
        action="store_true",
        help="Omit the country parameter and search every loaded country index.",
    )
    parser.add_argument(
        "--results-dir",
        default="benchmark-results",
        help="Directory for timestamped JSON results (set to an empty string to disable saving).",
    )
    parser.add_argument(
        "--monitor-host",
        help="Optional SSH host for server utilization sampling, e.g. peter@31.220.81.20.",
    )
    parser.add_argument(
        "--monitor-seconds",
        type=int,
        default=60,
        help="How long to sample utilization for each protocol run.",
    )
    parser.add_argument(
        "--monitor-interval",
        type=float,
        default=0.5,
        help="Utilization sample interval in seconds.",
    )
    parser.add_argument(
        "--country-codes",
        default="CZ,SK",
        help="Comma-separated country codes used in the DB sample query.",
    )
    return parser.parse_args()


def run_command(cmd: list[str], *, check: bool = True) -> subprocess.CompletedProcess[str]:
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if check and proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"command failed: {' '.join(cmd)}")
    return proc


def sample_addresses(args: argparse.Namespace) -> list[tuple[str, str]]:
    countries = [code.strip().upper() for code in args.country_codes.split(",") if code.strip()]
    if not countries:
        raise RuntimeError("at least one country code is required")

    country_sql = ",".join(f"'{code}'" for code in countries)
    sql = f"""
WITH sample AS (
    SELECT country_code,
           regexp_replace(full_address, ', ({'|'.join(countries)})$', '') AS query
    FROM addresses TABLESAMPLE SYSTEM ({args.sample_percent})
    WHERE country_code IN ({country_sql})
      AND is_active
      AND full_address IS NOT NULL
      AND length(full_address) >= 8
)
SELECT country_code || E'\\t' || query
FROM (
    SELECT DISTINCT country_code, query
    FROM sample
    WHERE query <> ''
) dedup
ORDER BY random()
LIMIT {args.sample_size};
"""
    proc = run_command(["psql", args.db_url, "-At", "-c", sql])
    items = []
    for line in proc.stdout.splitlines():
        if not line.strip():
            continue
        country, query = line.split("\t", 1)
        items.append((country, query))

    if len(items) != args.sample_size:
        raise RuntimeError(
            f"expected {args.sample_size} sampled addresses, got {len(items)}. "
            "Increase --sample-percent if needed."
        )
    return items


def load_queries_file(path: str, expected: int | None = None) -> list[tuple[str, str]]:
    items: list[tuple[str, str]] = []
    with open(path, encoding="utf-8") as handle:
        for lineno, raw_line in enumerate(handle, start=1):
            line = raw_line.rstrip("\n")
            if not line.strip():
                continue
            if "\t" not in line:
                raise RuntimeError(
                    f"{path}:{lineno}: expected 'COUNTRY<TAB>QUERY' format"
                )
            country, query = line.split("\t", 1)
            country = country.strip().upper()
            query = query.strip()
            if not country or not query:
                raise RuntimeError(
                    f"{path}:{lineno}: country and query must both be non-empty"
                )
            items.append((country, query))

    if expected is not None and len(items) != expected:
        raise RuntimeError(f"expected {expected} queries in {path}, got {len(items)}")
    return items


def prefixes(text: str) -> list[str]:
    return [text[:idx] for idx in range(1, len(text) + 1)]


def run_worker(
    item: tuple[str, str],
    *,
    protocol_flag: str,
    base_url: str,
    curl_bin: str,
    limit: int,
    timeout: int,
    api_key: str | None,
    origin: str | None,
    street_only: bool,
    all_countries: bool,
) -> dict:
    country, query = item
    urls = [f"{base_url}/health"]
    for part in prefixes(query):
        encoded = quote(part, safe="-_.~")
        url = f"{base_url}/search?q={encoded}&limit={limit}"
        if not all_countries:
            url += f"&country={country}"
        if api_key:
            url += f"&api_key={quote(api_key, safe='')}"
        if street_only:
            url += "&street_only"
        urls.append(url)

    cmd = [
        curl_bin,
        protocol_flag,
        "--silent",
        "--show-error",
    ]
    for idx, url in enumerate(urls):
        if idx > 0:
            cmd.append("--next")
        # curl's --next resets per-transfer options, including headers and --fail.
        # Add them for every URL so a 401/403 can never be recorded as a timing.
        cmd.extend(["--fail", "--max-time", str(timeout)])
        if origin:
            cmd.extend(["--header", f"Origin: {origin}"])
        cmd.extend(["--output", "/dev/null", "--write-out", "%{time_total}\n", url])

    started = time.perf_counter()
    proc = run_command(cmd)
    total_ms = (time.perf_counter() - started) * 1000.0

    timings = [float(line.strip()) * 1000.0 for line in proc.stdout.splitlines() if line.strip()]
    if len(timings) != len(urls):
        raise RuntimeError(f"expected {len(urls)} timings, got {len(timings)}")

    return {
        "country": country,
        "query": query,
        "chars": len(query),
        "prefixes": prefixes(query),
        "request_times_ms": timings[1:],
        "total_ms": total_ms,
    }


def run_protocol(
    name: str,
    *,
    items: list[tuple[str, str]],
    args: argparse.Namespace,
) -> dict:
    protocol_flag = "--http2" if name == "http2" else "--http3"
    wall_started = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        results = list(
            pool.map(
                lambda item: run_worker(
                    item,
                    protocol_flag=protocol_flag,
                    base_url=args.base_url,
                    curl_bin=args.curl_bin,
                    limit=args.limit,
                    timeout=args.timeout,
                    api_key=args.api_key,
                    origin=args.origin,
                    street_only=args.street_only,
                    all_countries=args.all_countries,
                ),
                items,
            )
        )
    wall_ms = (time.perf_counter() - wall_started) * 1000.0
    request_times = [timing for result in results for timing in result["request_times_ms"]]
    totals = [result["total_ms"] for result in results]
    times_by_prefix: dict[str, list[float]] = {}
    for result in results:
        for prefix, timing in zip(result["prefixes"], result["request_times_ms"], strict=True):
            times_by_prefix.setdefault(prefix, []).append(timing)

    return {
        "protocol": name,
        "workers": args.workers,
        "addresses_benchmarked": len(results),
        "total_request_count": len(request_times),
        "average_chars_per_address": sum(result["chars"] for result in results) / len(results),
        "wall_clock_ms": wall_ms,
        "per_request_ms": stats(request_times),
        "per_address_total_ms": stats(totals),
        "per_prefix_ms": {
            prefix: stats(times)
            for prefix, times in sorted(times_by_prefix.items())
        },
    }


def stats(values: list[float]) -> dict:
    ordered = sorted(values)
    return {
        "average": sum(values) / len(values),
        "median": statistics.median(values),
        "p95": percentile(ordered, 0.95),
        "p99": percentile(ordered, 0.99),
        "min": min(values),
        "max": max(values),
    }


def percentile(ordered_values: list[float], quantile: float) -> float:
    index = max(0, min(len(ordered_values) - 1, int(len(ordered_values) * quantile + 0.999999) - 1))
    return ordered_values[index]


def git_revision() -> str | None:
    proc = run_command(["git", "rev-parse", "HEAD"], check=False)
    return proc.stdout.strip() or None


def start_monitor(host: str, label: str, seconds: int, interval: float) -> tuple[int, str]:
    remote_path = f"/tmp/addresswise_{label}_monitor.log"
    samples = max(1, int(seconds / interval))
    script = (
        f"for i in $(seq 1 {samples}); do "
        f"ts=$(date +%s.%N); "
        f'echo "TS $ts" >> {remote_path}; '
        f'COLUMNS=200 ps -C addresswise -o pid,%cpu,%mem,rss,etime,cmd --no-headers >> {remote_path}; '
        f'free -m | awk \'NR==2 {{printf "MEM %s %s %s %s\\n", $2, $3, $4, $7}}\' >> {remote_path}; '
        f"sleep {interval}; "
        "done"
    )
    proc = subprocess.Popen(
        ["ssh", host, "/bin/bash", "-lc", script],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return proc.pid, remote_path


def collect_monitor(host: str, remote_path: str) -> dict | None:
    sql = rf"""
import json, statistics
from pathlib import Path
path = Path("{remote_path}")
lines = path.read_text().splitlines() if path.exists() else []
cpu=[]; rss=[]; mem=[]
for line in lines:
    if line.startswith("MEM "):
        parts=line.split()
        mem.append(float(parts[2]))
    elif line.strip() and not line.startswith("TS "):
        parts=line.split(None, 5)
        if len(parts) >= 4:
            try:
                cpu.append(float(parts[1]))
                rss.append(float(parts[3]))
            except ValueError:
                pass
summary = {{
    "samples": len(cpu),
    "addresswise_cpu_pct": {{
        "avg": sum(cpu)/len(cpu) if cpu else None,
        "median": statistics.median(cpu) if cpu else None,
        "max": max(cpu) if cpu else None,
    }},
    "addresswise_rss_mb": {{
        "avg": sum(rss)/len(rss)/1024.0 if rss else None,
        "median": statistics.median(rss)/1024.0 if rss else None,
        "max": max(rss)/1024.0 if rss else None,
    }},
    "system_mem_used_mb": {{
        "avg": sum(mem)/len(mem) if mem else None,
        "median": statistics.median(mem) if mem else None,
        "max": max(mem) if mem else None,
    }},
}}
print(json.dumps(summary))
"""
    proc = run_command(["ssh", host, "python3", "-c", sql])
    return json.loads(proc.stdout) if proc.stdout.strip() else None


def main() -> int:
    args = parse_args()
    if args.api_key and not args.origin:
        parsed_url = urlsplit(args.base_url)
        if not parsed_url.scheme or not parsed_url.netloc:
            raise RuntimeError("--base-url must include a scheme and host when using --api-key")
        args.origin = f"{parsed_url.scheme}://{parsed_url.netloc}"
    protocols = ["http2", "http3"] if args.protocol == "both" else [args.protocol]
    items = (
        load_queries_file(args.queries_file, expected=args.sample_size)
        if args.queries_file
        else sample_addresses(args)
    )

    output = {
        "created_at": datetime.now(UTC).isoformat(),
        "git_revision": git_revision(),
        "base_url": args.base_url,
        "workers": args.workers,
        "sample_size": args.sample_size,
        "authenticated": bool(args.api_key),
        "origin": args.origin,
        "street_only": args.street_only,
        "all_countries": args.all_countries,
        "sample_addresses": [
            {"country": country, "query": query, "chars": len(query)}
            for country, query in items[:5]
        ],
        "runs": {},
    }

    for protocol in protocols:
        monitor_path = None
        if args.monitor_host:
            _, monitor_path = start_monitor(
                args.monitor_host,
                protocol,
                args.monitor_seconds,
                args.monitor_interval,
            )

        run = run_protocol(protocol, items=items, args=args)
        if monitor_path:
            run["server_utilization"] = collect_monitor(args.monitor_host, monitor_path)
        output["runs"][protocol] = run

    if len(protocols) == 2:
        http2 = output["runs"]["http2"]
        http3 = output["runs"]["http3"]
        output["delta_ms"] = {
            "per_request_average": http3["per_request_ms"]["average"] - http2["per_request_ms"]["average"],
            "per_request_median": http3["per_request_ms"]["median"] - http2["per_request_ms"]["median"],
            "per_address_average": http3["per_address_total_ms"]["average"] - http2["per_address_total_ms"]["average"],
            "per_address_median": http3["per_address_total_ms"]["median"] - http2["per_address_total_ms"]["median"],
            "wall_clock": http3["wall_clock_ms"] - http2["wall_clock_ms"],
        }

    json.dump(output, sys.stdout, indent=2, ensure_ascii=False)
    sys.stdout.write("\n")
    if args.results_dir:
        results_dir = Path(args.results_dir)
        results_dir.mkdir(parents=True, exist_ok=True)
        revision = output["git_revision"] or "unknown"
        filename = f"{datetime.now(UTC).strftime('%Y%m%dT%H%M%S%fZ')}_{revision[:12]}.json"
        (results_dir / filename).write_text(
            json.dumps(output, indent=2, ensure_ascii=False) + "\n",
            encoding="utf-8",
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

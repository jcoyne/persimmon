#!/usr/bin/env python3
"""Measure adjacent IIIF tile requests across running image server instances.

Uses only the Python standard library. Supply one --url per instance, pointing
at the IIIF service root (for example http://127.0.0.1:3001/v3).
"""

import argparse
import concurrent.futures
import http.client
import json
import math
import subprocess
import threading
import time
import urllib.parse
import urllib.request


def tile_paths(service_url, identifier, width, height, tile_size, scale_factor):
    parsed = urllib.parse.urlsplit(service_url.rstrip("/"))
    if parsed.scheme not in ("http", "https") or not parsed.netloc:
        raise ValueError(f"invalid service URL: {service_url}")
    encoded = urllib.parse.quote(identifier, safe="")
    root = f"{parsed.path}/{encoded}"
    region_span = tile_size * scale_factor
    paths = []
    for y in range(0, height, region_span):
        for x in range(0, width, region_span):
            region_width = min(region_span, width - x)
            region_height = min(region_span, height - y)
            paths.append(
                f"{root}/{x},{y},{region_width},{region_height}/"
                f"!{tile_size},{tile_size}/0/default.jpg"
            )
    return parsed, paths


def metrics_url(parsed):
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, "/metrics", "", ""))


def read_metrics(parsed):
    with urllib.request.urlopen(metrics_url(parsed), timeout=10) as response:
        lines = response.read().decode("utf-8").splitlines()
    metrics = {}
    for line in lines:
        if line and not line.startswith("#"):
            name, value = line.split(None, 1)
            metrics[name] = float(value)
    return metrics


def rss_kib(pid):
    result = subprocess.run(
        ["ps", "-o", "rss=", "-p", str(pid)],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode or not result.stdout.strip():
        return None
    return int(result.stdout.strip())


def memory_mib(value):
    number = value.split(" / ", 1)[0].strip()
    for suffix, multiplier in [("GiB", 1024), ("MiB", 1), ("KiB", 1 / 1024),
                               ("GB", 1000), ("MB", 1000 / 1024), ("kB", 1 / 1024),
                               ("B", 1 / 1048576)]:
        if number.endswith(suffix):
            number = number[:-len(suffix)]
            return round(float(number) * multiplier, 2)
    raise ValueError(f"unrecognized Docker memory unit: {value}")


def sample_docker(containers, stop_event, samples, errors):
    while not stop_event.is_set():
        try:
            result = subprocess.run(
                ["docker", "stats", "--no-stream", "--format", "{{json .}}", *containers],
                capture_output=True, text=True, check=True, timeout=10,
            )
            for line in result.stdout.splitlines():
                item = json.loads(line)
                name = item["Name"]
                if name in samples:
                    samples[name].append({
                        "cpu_percent": float(item["CPUPerc"].removesuffix("%")),
                        "memory_mib": memory_mib(item["MemUsage"]),
                    })
        except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
            if len(errors) < 3:
                errors.append(str(error))
        stop_event.wait(1)


def request_one(connection, path):
    started = time.perf_counter()
    connection.request("GET", path)
    response = connection.getresponse()
    body = response.read()
    elapsed = time.perf_counter() - started
    if response.status != 200 or not response.getheader("Content-Type", "").startswith(
        "image/jpeg"
    ):
        raise RuntimeError(f"HTTP {response.status} for {path}")
    if not body:
        raise RuntimeError(f"empty image body for {path}")
    return elapsed


def connection_for(parsed):
    cls = http.client.HTTPSConnection if parsed.scheme == "https" else http.client.HTTPConnection
    return cls(parsed.hostname, parsed.port, timeout=30)


def worker(index, parsed, paths, start_event, deadline):
    start_event.wait()
    latencies = []
    errors = []
    count = 0
    connection = connection_for(parsed)
    try:
        while time.perf_counter() < deadline:
            path = paths[(index + count) % len(paths)]
            try:
                latencies.append(request_one(connection, path))
            except (OSError, http.client.HTTPException, RuntimeError) as error:
                if len(errors) < 10:
                    errors.append(str(error))
                connection.close()
                connection = connection_for(parsed)
            count += 1
    finally:
        connection.close()
    return latencies, count, errors


def percentile(values, percentage):
    if not values:
        return None
    position = math.ceil(len(values) * percentage / 100) - 1
    return round(values[max(0, position)] * 1000, 2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", action="append", required=True, help="IIIF service root; repeat per instance")
    parser.add_argument("--identifier", required=True, help="decoded source identifier")
    parser.add_argument("--width", type=int, required=True)
    parser.add_argument("--height", type=int, required=True)
    parser.add_argument("--tile-size", type=int, default=256)
    parser.add_argument("--scale-factor", type=int, default=1)
    parser.add_argument("--max-tiles", type=int, help="use only the first N adjacent tiles")
    parser.add_argument("--connections", type=int, default=32)
    parser.add_argument("--seconds", type=float, default=30)
    parser.add_argument("--warm", action="store_true", help="request every tile on every instance before measuring")
    parser.add_argument("--no-metrics", action="store_true", help="skip /metrics for servers without that endpoint")
    parser.add_argument("--pid", action="append", type=int, default=[], help="server PID to sample RSS; repeat as needed")
    parser.add_argument("--docker-container", action="append", default=[],
                        help="sample Docker CPU and memory; repeat for each container")
    parser.add_argument("--output", help="optional JSON report file")
    args = parser.parse_args()
    if min(args.width, args.height, args.tile_size, args.scale_factor, args.connections) <= 0:
        parser.error("dimensions, tile size, scale factor, and connections must be positive")
    if args.seconds <= 0:
        parser.error("seconds must be positive")
    if args.max_tiles is not None and args.max_tiles <= 0:
        parser.error("max-tiles must be positive")

    instances = [
        tile_paths(url, args.identifier, args.width, args.height, args.tile_size, args.scale_factor)
        for url in args.url
    ]
    if args.max_tiles is not None:
        instances = [(parsed, paths[: args.max_tiles]) for parsed, paths in instances]
    if args.warm:
        for parsed, paths in instances:
            connection = connection_for(parsed)
            try:
                for path in paths:
                    request_one(connection, path)
            finally:
                connection.close()

    before = [{} for _ in instances] if args.no_metrics else [read_metrics(parsed) for parsed, _ in instances]
    start_event = threading.Event()
    started = time.perf_counter()
    deadline = started + args.seconds
    peak_rss = {str(pid): rss_kib(pid) for pid in args.pid}
    docker_samples = {name: [] for name in args.docker_container}
    docker_errors = []
    docker_stop = threading.Event()
    docker_thread = None
    if args.docker_container:
        docker_thread = threading.Thread(
            target=sample_docker,
            args=(args.docker_container, docker_stop, docker_samples, docker_errors),
            daemon=True,
        )
        docker_thread.start()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.connections) as pool:
        futures = [
            pool.submit(worker, index, *instances[index % len(instances)], start_event, deadline)
            for index in range(args.connections)
        ]
        start_event.set()
        while time.perf_counter() < deadline:
            for pid in args.pid:
                current = rss_kib(pid)
                if current is not None:
                    key = str(pid)
                    peak_rss[key] = max(peak_rss[key] or 0, current)
            time.sleep(min(1.0, max(0.0, deadline - time.perf_counter())))
        results = [future.result() for future in futures]
    elapsed = time.perf_counter() - started
    docker_stop.set()
    if docker_thread is not None:
        docker_thread.join(timeout=2)
    after = [{} for _ in instances] if args.no_metrics else [read_metrics(parsed) for parsed, _ in instances]
    latencies = sorted(value for result in results for value in result[0])
    errors = [error for result in results for error in result[2]]
    successes = len(latencies)
    attempts = sum(result[1] for result in results)
    metrics = []
    for (parsed, _), old, new in zip(instances, before, after):
        metrics.append(
            {
                "instance": parsed.netloc,
                "delta": {
                    key: round(value - old.get(key, 0.0), 6)
                    for key, value in new.items()
                    if key.endswith("_total")
                },
                "source_cache_bytes": new.get("persimmon_source_cache_bytes"),
            }
        )
    report = {
        "instances": len(instances),
        "tile_urls_per_instance": len(instances[0][1]),
        "connections": args.connections,
        "warm": args.warm,
        "elapsed_seconds": round(elapsed, 3),
        "requests": attempts,
        "successes": successes,
        "errors": attempts - successes,
        "requests_per_second": round(attempts / elapsed, 2),
        "latency_ms": {
            "p50": percentile(latencies, 50),
            "p95": percentile(latencies, 95),
            "p99": percentile(latencies, 99),
        },
        "metrics": metrics,
        "metrics_collected": not args.no_metrics,
        "peak_rss_kib_by_pid": peak_rss,
        "docker_stats": {
            name: {
                "samples": len(samples),
                "cpu_percent_avg": round(sum(item["cpu_percent"] for item in samples) / len(samples), 2)
                if samples else None,
                "cpu_percent_peak": max((item["cpu_percent"] for item in samples), default=None),
                "memory_mib_peak": max((item["memory_mib"] for item in samples), default=None),
            }
            for name, samples in docker_samples.items()
        },
        "docker_stats_errors": docker_errors,
        "error_samples": errors[:10],
    }
    encoded = json.dumps(report, indent=2, sort_keys=True)
    print(encoded)
    if args.output:
        with open(args.output, "w", encoding="utf-8") as report_file:
            report_file.write(encoded + "\n")
    if report["errors"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()

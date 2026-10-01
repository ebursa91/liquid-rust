#!/usr/bin/env python3
"""Measure verified Horizon HTTP throughput with identical bounded host plumbing."""
import argparse
import asyncio
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import random
import shutil
import signal
import statistics
import struct
import subprocess
import sys
import time

import benchmark as common

MAX_HEADER = 4096
VARIANTS = common.VARIANTS


def emit(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


def read_json(path):
    return json.loads(Path(path).read_text())


def write_json(path, value):
    safe_file(path)
    Path(path).write_text(json.dumps(value, indent=2, allow_nan=False) + "\n")


def safe_file(path):
    path = Path(path)
    common.require(not path.is_symlink() and (not path.exists() or path.is_file()), f"regular output file required: {path}")
    return path


def percentiles(values):
    if not values:
        return {"count": 0, "p50_ms": None, "p95_ms": None, "p99_ms": None, "max_ms": None}
    return {"count": len(values), "p50_ms": statistics.median(values),
            "p95_ms": common.quantile(values, .95), "p99_ms": common.quantile(values, .99), "max_ms": max(values)}


def process_snapshot(pid):
    """Linux cumulative CPU seconds and current/peak RSS, never allocation bytes."""
    try:
        stat = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        ticks = os.sysconf("SC_CLK_TCK")
        status = Path(f"/proc/{pid}/status").read_text().splitlines()
        fields = {line.split(":", 1)[0]: line.split(":", 1)[1].strip() for line in status if ":" in line}
        return {"pid": pid, "cpu_seconds": (int(stat[11]) + int(stat[12])) / ticks,
                "rss_kib": int(fields["VmRSS"].split()[0]), "peak_rss_kib": int(fields["VmHWM"].split()[0])}
    except (OSError, KeyError, ValueError, IndexError):
        return {"pid": pid, "cpu_seconds": None, "rss_kib": None, "peak_rss_kib": None}


def checked_ready(ready, config):
    common.require(ready.get("action") == "ready" and ready.get("schema_version") == 1, "invalid worker READY")
    common.require({key: ready.get(key) for key in ("html", "css")} == config["expected"], "worker READY fingerprints differ")
    if config["variant"] == "control":
        common.require(ready.get("engine") == "transport-control" and ready.get("response_cache") is True and ready.get("warmup") == 0, "wrong cached transport control")
        return
    common.require(ready.get("response_cache") is False and ready.get("warmup") == config["warmup"], "worker cache/warmup contract differs")
    common.require(ready.get("correctness_verified") is True and ready.get("scope") == "page" and ready.get("page") == "index", "worker correctness/scope differs")
    common.require(ready.get("theme_sha") == common.THEME_SHA and ready.get("fixture_sha256") == config["fixture_sha256"], "worker input pins differ")
    if config["variant"] == "rust":
        common.require(ready.get("engine") == "liquid-rust" and ready.get("build_profile") == "release", "release Rust worker required")
    else:
        common.require(ready.get("engine") == "ruby", "wrong Ruby worker engine")
        common.require(ready.get("liquid_sha") == common.LIQUID_SHA and ready.get("ruby_version") == config["ruby_version"], "Ruby worker source/runtime differs")
        common.require(ready.get("yjit_enabled") is (config["variant"] != "ruby"), "Ruby worker YJIT mode differs")
        common.require(ready.get("execution_mode") == ("fiber" if config["variant"] == "ruby_fiber_yjit" else "direct"), "Ruby worker execution mode differs")
        common.require(ready.get("dependencies") == config["dependencies"], "Ruby worker dependencies differ")


class Worker:
    def __init__(self, process, ready):
        self.process, self.ready = process, ready
        self.requests = 0

    async def render(self, request_id, expected, timeout):
        process = self.process
        process.stdin.write((json.dumps({"action": "render", "request_id": request_id}) + "\n").encode())
        await process.stdin.drain()
        async with asyncio.timeout(timeout):
            line = await process.stdout.readline()
            common.require(line and len(line) <= MAX_HEADER, "missing/oversized worker response header")
            reply = json.loads(line)
            common.require(reply.get("request_id") == request_id, "worker response ID differs")
            common.require(reply.get("error") is None, f"worker failed: {reply.get('error')}")
            lengths = tuple(reply.get(key) for key in ("html_bytes", "css_bytes"))
            common.require(lengths == tuple(expected[key]["bytes"] for key in ("html", "css")), "worker response lengths differ")
            html = await process.stdout.readexactly(lengths[0])
            css = await process.stdout.readexactly(lengths[1])
        self.requests += 1
        return html, css


async def serve(config, report_path):
    """One common asyncio HTTP/1.1 front end; renderer pool uses identical pipes."""
    available = asyncio.Queue()
    workers = []
    counters = Counter()
    stopped = asyncio.Event()
    clients = set()
    next_id = 0
    fatal = []
    for index, command in enumerate(config["commands"]):
        log = open(safe_file(Path(report_path).parent / f"worker-{index}.log"), "wb")
        try:
            child = await asyncio.create_subprocess_exec(*command, stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
                                                       stderr=log, limit=MAX_HEADER + 1)
        finally:
            log.close()
        try:
            line = await asyncio.wait_for(child.stdout.readline(), config["startup_timeout"])
            common.require(line and len(line) <= MAX_HEADER, "missing/oversized worker READY")
            ready = json.loads(line)
            checked_ready(ready, config)
        except Exception:
            if child.returncode is None:
                child.kill()
            await child.wait()
            for worker in workers:
                worker.process.kill()
                await worker.process.wait()
            raise
        worker = Worker(child, ready)
        workers.append(worker)
        available.put_nowait(worker)

    async def reply(writer, status, body=b"", close=False):
        reason = {200: "OK", 400: "Bad Request", 404: "Not Found", 500: "Internal Server Error", 503: "Service Unavailable"}[status]
        writer.write(f"HTTP/1.1 {status} {reason}\r\nContent-Type: application/octet-stream\r\nContent-Length: {len(body)}\r\nConnection: {'close' if close else 'keep-alive'}\r\n\r\n".encode())
        writer.write(body)
        await writer.drain()

    async def handle(reader, writer):
        nonlocal next_id
        task = asyncio.current_task()
        clients.add(task)
        try:
            while not stopped.is_set():
                header = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), config.get("idle_timeout", 60))
                if len(header) > MAX_HEADER:
                    await reply(writer, 400, close=True)
                    break
                lines = header.decode("ascii").split("\r\n")
                if lines[0] != "GET /render HTTP/1.1" or any(line.lower().startswith(("content-length:", "transfer-encoding:")) for line in lines[1:] if line):
                    await reply(writer, 404, close=True)
                    break
                counters["offered"] += 1
                if counters["in_flight"] >= config["queue_cap"] or fatal:
                    counters["rejected"] += 1
                    await reply(writer, 503)
                    continue
                counters["in_flight"] += 1
                counters["admitted"] += 1
                next_id += 1
                request_id = next_id
                worker = None
                try:
                    try:
                        worker = await asyncio.wait_for(available.get(), config["request_timeout"])
                    except asyncio.TimeoutError:
                        counters["queue_timeout"] += 1
                        try:
                            await reply(writer, 503)
                        except ConnectionError:
                            counters["client_disconnect"] += 1
                            break
                        continue
                    html, css = await worker.render(request_id, config["expected"], config["request_timeout"])
                    # This full request performs rendering and IPC. No response cache.
                    body = struct.pack(">QQ", len(html), len(css)) + html + css
                    counters["rendered"] += 1
                    counters["payload_bytes"] += len(body)
                    available.put_nowait(worker)
                    worker = None
                    try:
                        await reply(writer, 200, body)
                        counters["sent"] += 1
                    except ConnectionError:
                        counters["client_disconnect"] += 1
                        break
                except Exception as error:
                    counters["failed"] += 1
                    fatal.append(str(error))
                    await reply(writer, 500)
                finally:
                    counters["in_flight"] -= 1
                    if worker is not None and worker.process.returncode is None:
                        worker.process.kill()
                if any(line.lower() == "connection: close" for line in lines[1:]):
                    break
        except (asyncio.IncompleteReadError, ConnectionError, asyncio.TimeoutError, UnicodeError, asyncio.LimitOverrunError):
            counters["connection_closed"] += 1
        finally:
            writer.close()
            try:
                await writer.wait_closed()
            except ConnectionError:
                pass
            clients.discard(task)

    server = await asyncio.start_server(handle, "127.0.0.1", 0, limit=MAX_HEADER + 1)
    snapshots = {"frontend": process_snapshot(os.getpid()), "workers": [process_snapshot(w.process.pid) for w in workers]}
    emit({"action": "server_ready", "port": server.sockets[0].getsockname()[1], "pid": os.getpid(),
          "workers": [dict(pid=w.process.pid, ready=w.ready) for w in workers], "resources_start": snapshots})
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, stopped.set)
    await stopped.wait()
    server.close()
    await server.wait_closed()
    # A client deadline may expire while its admitted request is still rendering.
    # Finish those frames before stopping workers so overload is not a fatal event.
    _, pending = await asyncio.wait(list(clients), timeout=config["request_timeout"] * 2 + 1) if clients else (set(), set())
    for task in pending:
        task.cancel()
    await asyncio.gather(*pending, return_exceptions=True)
    final = {"frontend": process_snapshot(os.getpid()), "workers": [process_snapshot(w.process.pid) for w in workers]}
    for worker in workers:
        if worker.process.returncode is None:
            worker.process.stdin.close()
        try:
            await asyncio.wait_for(worker.process.wait(), 10)
        except asyncio.TimeoutError:
            worker.process.kill()
            await worker.process.wait()
    write_json(report_path, {"counts": dict(counters), "fatal": fatal, "resources_start": snapshots, "resources_end": final,
                             "worker_request_counts": [w.requests for w in workers], "worker_exit_codes": [w.process.returncode for w in workers]})


class Connection:
    def __init__(self, port, expected):
        self.port, self.expected = port, expected
        self.reader = self.writer = None

    async def close(self):
        if self.writer:
            self.writer.close()
            try:
                await self.writer.wait_closed()
            except ConnectionError:
                pass
        self.reader = self.writer = None

    async def connect(self):
        if self.writer is None:
            self.reader, self.writer = await asyncio.open_connection("127.0.0.1", self.port, limit=MAX_HEADER + 1)

    async def request(self):
        await self.connect()
        self.writer.write(b"GET /render HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n")
        await self.writer.drain()
        header = await self.reader.readuntil(b"\r\n\r\n")
        common.require(len(header) <= MAX_HEADER, "oversized HTTP header")
        lines = header.decode("ascii").split("\r\n")
        status = int(lines[0].split()[1])
        fields = dict(line.lower().split(":", 1) for line in lines[1:] if ":" in line)
        size = int(fields["content-length"])
        common.require(0 <= size <= 16 + sum(x["bytes"] for x in self.expected.values()), "invalid HTTP payload length")
        body = await self.reader.readexactly(size)
        if status != 200:
            return f"http_{status}"
        common.require(size >= 16, "missing HTML/CSS lengths")
        html, css = struct.unpack(">QQ", body[:16])
        common.require((html, css) == tuple(self.expected[k]["bytes"] for k in ("html", "css")), "HTTP HTML/CSS lengths differ")
        view = memoryview(body)
        for kind, data in (("html", view[16:16 + html]), ("css", view[16 + html:])):
            common.require(hashlib.sha256(data).hexdigest() == self.expected[kind]["sha256"], f"HTTP {kind} digest differs")
        return None


async def load_window(config, mode, duration, concurrency, rate=None, connections=None):
    pool = asyncio.Queue()
    owns_connections = connections is None
    connections = connections or [Connection(config["port"], config["expected"]) for _ in range(concurrency)]
    for connection in connections:
        pool.put_nowait(connection)
    samples, successful_samples, failed_samples, timeout_samples, lags = [], [], [], [], []
    counts = Counter()
    start = time.perf_counter()
    end = start + duration
    active = set()

    async def one(intended):
        connection = None
        counts["started"] += 1
        try:
            async with asyncio.timeout(max(0, intended + config["request_timeout"] - time.perf_counter())):
                connection = await pool.get()
                counts["sent"] += 1
                error = await connection.request()
                completed = time.perf_counter()
                elapsed = (completed - intended) * 1000
                samples.append(elapsed)
                if error:
                    counts[error] += 1
                    failed_samples.append(elapsed)
                else:
                    counts["successful"] += 1
                    if completed <= end:
                        counts["successful_in_window"] += 1
                    successful_samples.append(elapsed)
        except asyncio.TimeoutError:
            counts["timeout"] += 1
            timeout_samples.append((time.perf_counter() - intended) * 1000)
            if connection:
                await connection.close()
        except (ValueError, KeyError, UnicodeError, struct.error) as error:
            counts["correctness_error"] += 1
            config.setdefault("fatal", []).append(str(error))
            if connection:
                await connection.close()
        except (OSError, asyncio.IncompleteReadError, asyncio.LimitOverrunError):
            counts["connection_error"] += 1
            if connection:
                await connection.close()
        finally:
            if connection:
                pool.put_nowait(connection)

    if mode == "closed":
        async def stream():
            while time.perf_counter() < end:
                counts["offered"] += 1
                await one(time.perf_counter())
        await asyncio.gather(*(stream() for _ in range(concurrency)))
    else:
        planned = math.ceil(rate * duration)
        for index in range(planned):
            intended = start + index / rate
            delay = intended - time.perf_counter()
            if delay > 0:
                await asyncio.sleep(delay)
            now = time.perf_counter()
            counts["offered"] += 1
            lags.append(max(0, now - intended) * 1000)
            if now >= end or now - intended > 1 / rate:
                counts["scheduler_drop"] += 1
            elif len(active) >= config["max_outstanding"]:
                counts["queue_drop"] += 1
            else:
                task = asyncio.create_task(one(intended))
                active.add(task)
                task.add_done_callback(active.discard)
        await asyncio.gather(*active)
    drained = time.perf_counter()
    if owns_connections:
        await asyncio.gather(*(connection.close() for connection in connections))
    counts = {key: counts[key] for key in ("offered", "started", "sent", "successful", "successful_in_window", "http_503", "http_500", "timeout", "connection_error", "correctness_error", "queue_drop", "scheduler_drop")}
    common.require(counts["offered"] == counts["started"] + counts["queue_drop"] + counts["scheduler_drop"], "offered request accounting differs")
    common.require(counts["started"] == counts["successful"] + sum(counts[k] for k in ("http_503", "http_500", "timeout", "connection_error", "correctness_error")), "completed request accounting differs")
    return {"mode": mode, "concurrency": concurrency, "offered_rate_rps": rate, "measurement_seconds": duration,
            "drain_seconds": max(0, drained - end), "wall_seconds_including_drain": drained - start,
            "achieved_rps": counts["successful_in_window"] / duration, "counts": counts,
            "latency": percentiles(samples), "latency_samples_ms": samples,
            "successful_latency": percentiles(successful_samples), "successful_latency_samples_ms": successful_samples,
            "failed_response_latency": percentiles(failed_samples), "timeout_latency": percentiles(timeout_samples),
            "successful_during_drain": counts["successful"] - counts["successful_in_window"],
            "scheduler_lag": percentiles(lags), "fatal": config.get("fatal", []),
            "scheduler_late_drop_threshold_ms": 1000 / rate if rate else None,
            "latency_contract": "intended arrival to full body (successful bodies verified), including connection-pool queue; completed requests offered in window with bounded drain; timeouts and drops separately",
            "coordinated_omission": mode == "closed"}


async def load(config, output):
    before = process_snapshot(os.getpid())
    connections = [Connection(config["port"], config["expected"]) for _ in range(config["concurrency"])]
    try:
        async with asyncio.timeout(config["request_timeout"]):
            await asyncio.gather(*(connection.connect() for connection in connections))
        # Open-loop connections must not all issue a closed-loop burst during ramp.
        ramp = await load_window(config, "closed", config["ramp_seconds"], config.get("ramp_concurrency", config["concurrency"]), connections=connections)
        common.require(not ramp["fatal"], "ramp response validation failed")
        result = await load_window(config, config["mode"], config["duration"], config["concurrency"], config.get("rate"), connections)
    finally:
        await asyncio.gather(*(connection.close() for connection in connections))
    result.update(resources_start=before, resources_end=process_snapshot(os.getpid()), ramp=ramp)
    write_json(output, result)
    emit({"action": "load_complete", "achieved_rps": result["achieved_rps"], "counts": result["counts"]})


def dummy(config):
    """Transport-only calibration explicitly caches bytes; engines never do."""
    html, css = (Path(config[k]).read_bytes() for k in ("html_path", "css_path"))
    emit({"action": "ready", "schema_version": 1, "engine": "transport-control", "response_cache": True,
          "warmup": 0, "html": common.digest_bytes(html), "css": common.digest_bytes(css)})
    output = sys.stdout.buffer
    for line in sys.stdin.buffer:
        common.require(len(line) <= MAX_HEADER, "oversized control request")
        request = json.loads(line)
        common.require(request["action"] == "render", "wrong control action")
        emit({"request_id": request["request_id"], "html_bytes": len(html), "css_bytes": len(css), "error": None})
        output.write(html)
        output.write(css)
        output.flush()


def topology(cpus):
    allowed = os.sched_getaffinity(0)
    identities = []
    for cpu in cpus:
        common.require(cpu in allowed, f"CPU {cpu} unavailable")
        root = Path(f"/sys/devices/system/cpu/cpu{cpu}/topology")
        identities.append((int((root / "physical_package_id").read_text()), int((root / "core_id").read_text())))
    common.require(len(set(identities)) == len(identities), "worker/frontend/client CPUs must be distinct physical cores")
    return [{"cpu": cpu, "package": identity[0], "core": identity[1]} for cpu, identity in zip(cpus, identities)]


def worker_command(variant, args):
    common_args = ["--theme-root", str(args.theme_root), "--fixture", str(args.fixture), "--warmup", str(args.warmup)]
    if variant == "rust":
        return [str(args.rust_binary), "--serve-stdio", *common_args, "--scope", "page", "--page", "index"]
    return [str(args.ruby), "--disable-yjit" if variant == "ruby" else "--yjit", "-rbundler/setup", str(args.ruby_root / "benchmark/serve_worker.rb"),
            "--liquid-root", str(args.liquid_root), *common_args, "--mode", "fiber" if variant == "ruby_fiber_yjit" else "direct"]


async def subprocess_checked(command, log, timeout, env=None):
    child = await asyncio.create_subprocess_exec(*command, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT, env=env, start_new_session=True)
    try:
        try:
            output, _ = await asyncio.wait_for(child.communicate(), timeout)
        except asyncio.TimeoutError:
            kill_group(child.pid)
            output, _ = await child.communicate()
            safe_file(log).write_bytes(output)
            raise ValueError(f"subprocess timeout: {log}")
    finally:
        kill_group(child.pid)
    safe_file(log).write_bytes(output)
    common.require(child.returncode == 0, f"subprocess failed: {log}")


def kill_group(pid):
    try:
        os.killpg(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


async def orchestrate(args):
    output = args.output_dir.resolve()
    roots = [args.ruby_root, args.rust_root, args.liquid_root, args.theme_root]
    for root in roots:
        common.require(not output.is_relative_to(root.resolve()), "output must be outside all source checkouts")
    for path in (args.fixture, args.rust_binary, args.ruby, args.expected_comparison):
        common.require(not path.resolve().is_relative_to(output), "output must not contain an input")
    common.safe_directory(output, output)
    marker = output / "rps.json"
    common.require(not marker.is_symlink(), "success marker must not be a symlink")
    marker.unlink(missing_ok=True)
    common.require(args.ruby_exists, "Ruby executable missing")
    common.require(args.taskset is not None and sys.platform == "linux", "Linux taskset and /proc topology required")
    common.require(args.worker_counts and all(x in (1, 4) for x in args.worker_counts) and len(set(args.worker_counts)) == len(args.worker_counts), "worker counts must be distinct 1/4")
    common.require(all(x > 0 for x in [*(args.concurrency or []), *args.concurrency_multipliers, *args.control_concurrency, args.open_connections, args.queue_cap, args.max_outstanding]), "positive concurrency/queue counts required")
    common.require(all(math.isfinite(x) and x > 0 for x in [*args.rates_one, *args.rates_four, args.headroom, args.startup_timeout]), "positive finite offered rates/timeouts required")
    common.require(args.duration > 0 and args.ramp >= 0 and args.batches > 0 and args.warmup > 0 and args.timeout > 0, "invalid measurement counts")
    common.require(all(math.isfinite(x) for x in (args.duration, args.ramp, args.timeout)), "finite durations required")
    common.require(args.headroom >= 5, "transport headroom must be at least five times measured throughput")
    cpus = args.worker_cpus
    common.require(len(cpus) >= max(args.worker_counts), "insufficient worker CPUs")
    hardware = topology([*cpus, args.frontend_cpu, args.client_cpu])
    sources = {key: common.repository(root.resolve(), common.LIQUID_SHA if key == "liquid" else common.THEME_SHA if key == "theme" else None)
               for key, root in zip(("ruby", "rust", "liquid", "theme"), roots)}
    inputs = {"fixture": common.fingerprint(args.fixture), "binary": common.fingerprint(args.rust_binary), "ruby": common.fingerprint(args.ruby)}
    common.require(common.fingerprint(args.ruby_root / "fixtures/store.json") == inputs["fixture"], "public and shared fixture bytes differ")
    subprocess.run([sys.executable, str(args.rust_root / "compat/horizon/validate_fixture.py"), str(args.fixture)], check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    expected_manifest = read_json(args.expected_comparison)
    common.require(expected_manifest["fixture"] == inputs["fixture"] and expected_manifest["scope"] == "page" and expected_manifest["theme_sha"] == common.THEME_SHA and expected_manifest["ruby_liquid_sha"] == common.LIQUID_SHA, "expected comparison differs")
    expected = {key: expected_manifest["artifacts"][name] for key, name in common.ARTIFACTS.items()}
    env = os.environ.copy()
    for key in ("RUBYLIB", "RUBYOPT", "RUBY_YJIT_ENABLE", "GEM_HOME", "GEM_PATH", "BUNDLE_PATH"):
        env.pop(key, None)
    env["BUNDLE_GEMFILE"] = str(args.ruby_root / "Gemfile")
    if args.ruby_gem_home:
        env["GEM_HOME"] = str(args.ruby_gem_home)
    if args.ruby_gem_path:
        env["GEM_PATH"] = args.ruby_gem_path
    args.expected_ruby_version = subprocess.check_output([str(args.ruby), "--disable-yjit", "--version"], env=env, text=True).split()[1]
    args.scope, args.page = "page", "index"
    # Independent untimed original Ruby oracle also supplies exact calibration bytes.
    preflight = output / "oracle"
    common.fresh_output(preflight, output)
    await subprocess_checked(common.command("ruby", preflight, args), preflight / "process.log", args.startup_timeout, env)
    common.require(common.artifact_digests(preflight, expected) == expected, "oracle output differs")
    report = read_json(preflight / "report.json")
    common.validate_report(report, "ruby", args, inputs["fixture"]["sha256"], False, expected)
    base = {"expected": expected, "fixture_sha256": inputs["fixture"]["sha256"], "ruby_version": args.expected_ruby_version,
            "dependencies": report["dependencies"], "warmup": args.warmup, "startup_timeout": args.startup_timeout,
            "request_timeout": args.timeout, "queue_cap": args.queue_cap, "idle_timeout": max(60, args.duration + args.ramp + args.timeout * 3)}
    control = output / "control-config.json"
    write_json(control, dict(html_path=str(preflight / "index.html"), css_path=str(preflight / "styles.css"), warmup=args.warmup))
    script = Path(__file__).resolve()
    rng = random.Random(args.seed)
    runs, servers = [], []
    started = time.time()
    host_start = common.host()
    for batch in range(args.batches):
        cases = [(count, variant) for count in args.worker_counts for variant in (*VARIANTS, "control")]
        rng.shuffle(cases)
        for count, variant in cases:
            directory = common.safe_directory(output / f"batch-{batch:02d}" / f"{variant}-{count}", output)
            commands = []
            for cpu in cpus[:count]:
                command = [sys.executable, str(script), "--role", "control", "--config", str(control)] if variant == "control" else worker_command(variant, args)
                commands.append([args.taskset, "-c", str(cpu), *command])
            config = dict(base, variant=variant, commands=commands)
            config_path = directory / "server-config.json"
            write_json(config_path, config)
            server_log = open(safe_file(directory / "server.log"), "wb")
            server = await asyncio.create_subprocess_exec(args.taskset, "-c", str(args.frontend_cpu), sys.executable, str(script), "--role", "server", "--config", str(config_path), "--report", str(directory / "server-report.json"), stdout=asyncio.subprocess.PIPE, stderr=server_log, env=env, start_new_session=True)
            server_log.close()
            try:
                ready_line = await asyncio.wait_for(server.stdout.readline(), args.startup_timeout * count)
                common.require(ready_line, f"server failed: {directory / 'server.log'}")
                ready = json.loads(ready_line)
                common.require(ready["action"] == "server_ready", "invalid front-end readiness")
                levels = args.control_concurrency if variant == "control" else (args.concurrency or [count * m for m in args.concurrency_multipliers])
                windows = [("closed", value) for value in levels]
                if variant != "control":
                    windows += [("open", rate) for rate in (args.rates_one if count == 1 else args.rates_four)]
                rng.shuffle(windows)
                for index, (mode, value) in enumerate(windows):
                    load_config = {"port": ready["port"], "expected": expected, "mode": mode, "duration": args.duration,
                                   "ramp_seconds": args.ramp, "concurrency": value if mode == "closed" else args.open_connections,
                                   "ramp_concurrency": value if mode == "closed" else count,
                                   "rate": value if mode == "open" else None, "request_timeout": args.timeout,
                                   "max_outstanding": args.max_outstanding}
                    load_path = directory / f"load-{index}.json"
                    write_json(load_path, load_config)
                    result_path = directory / f"result-{index}.json"
                    resources_before = {"frontend": process_snapshot(server.pid), "workers": [process_snapshot(w["pid"]) for w in ready["workers"]]}
                    load_before = common.host()["load_average"]
                    await subprocess_checked([args.taskset, "-c", str(args.client_cpu), sys.executable, str(script), "--role", "client", "--config", str(load_path), "--report", str(result_path)], directory / f"client-{index}.log", args.duration + args.ramp + args.timeout * 3 + 20)
                    result = read_json(result_path)
                    common.require(not result["fatal"] and not result["counts"]["correctness_error"] and not result["counts"]["http_500"], "render correctness/protocol failure")
                    result.update(batch=batch, variant=variant, workers=count, worker_cpus=cpus[:count], frontend_cpu=args.frontend_cpu,
                                  client_cpu=args.client_cpu, load_before=load_before, load_after=common.host()["load_average"],
                                  resource_snapshot_scope="client setup, HTTP ramp, measured window and bounded drain",
                                  resources_before=resources_before, resources_after={"frontend": process_snapshot(server.pid), "workers": [process_snapshot(w["pid"]) for w in ready["workers"]]})
                    runs.append(result)
                    print(f"batch {batch + 1}/{args.batches} {variant}/{count} {mode}/{value}: {result['achieved_rps']:.2f} RPS", file=sys.stderr, flush=True)
            finally:
                if server.returncode is None:
                    server.terminate()
                try:
                    await asyncio.wait_for(server.wait(), args.timeout + 15)
                except asyncio.TimeoutError:
                    kill_group(server.pid)
                    await server.wait()
                finally:
                    # Kill worker descendants even when their front end exited early.
                    kill_group(server.pid)
            common.require(server.returncode == 0, f"front end failed: {directory / 'server.log'}")
            server_report = read_json(directory / "server-report.json")
            common.require(not server_report["fatal"] and not any(server_report["worker_exit_codes"]), "worker failed")
            servers.append(dict(batch=batch, variant=variant, workers=count, ready=ready, report=server_report))
    headroom = []
    for count in args.worker_counts:
        engines = [x["achieved_rps"] for x in runs if x["workers"] == count and x["variant"] != "control"]
        controls = [x["achieved_rps"] for x in runs if x["workers"] == count and x["variant"] == "control"]
        ratio = statistics.median(controls) / max(engines) if max(engines) > 0 else None
        headroom.append({"workers": count, "median_control_rps": statistics.median(controls), "max_observed_engine_rps": max(engines),
                         "ratio": ratio, "required_ratio": args.headroom, "passed": ratio is not None and ratio >= args.headroom})
    summary = []
    groups = sorted({(r["workers"], r["variant"], r["mode"], r["concurrency"], r["offered_rate_rps"] or 0) for r in runs})
    for count, variant, mode, concurrency, rate in groups:
        windows = [r for r in runs if (r["workers"], r["variant"], r["mode"], r["concurrency"], r["offered_rate_rps"] or 0) == (count, variant, mode, concurrency, rate)]
        rates = [r["achieved_rps"] for r in windows]
        summary.append({"workers": count, "variant": variant, "mode": mode, "concurrency": concurrency,
                        "offered_rate_rps": rate or None, "batch_rps": rates, "median_rps": statistics.median(rates),
                        "min_rps": min(rates), "max_rps": max(rates), "descriptive_batch_bootstrap_95_interval_rps": common.interval(rates, args.seed),
                        "latency": percentiles([t for r in windows for t in r["latency_samples_ms"]]),
                        "successful_latency": percentiles([t for r in windows for t in r["successful_latency_samples_ms"]]),
                        "counts": {key: sum(r["counts"][key] for r in windows) for key in windows[0]["counts"]}})
    for key, root in zip(("ruby", "rust", "liquid", "theme"), roots):
        common.require(common.repository(root.resolve(), common.LIQUID_SHA if key == "liquid" else common.THEME_SHA if key == "theme" else None) == sources[key], "source changed during RPS run")
    common.require({"fixture": common.fingerprint(args.fixture), "binary": common.fingerprint(args.rust_binary), "ruby": common.fingerprint(args.ruby)} == inputs, "input changed during RPS run")
    result = {"schema_version": 1, "correctness_verified": True, "transport_headroom_passed": all(x["passed"] for x in headroom),
              "publication_valid": all(x["passed"] for x in headroom), "started_unix": started, "completed_unix": time.time(),
              "configuration": {k: getattr(args, k) for k in ("duration", "ramp", "batches", "warmup", "seed", "worker_counts", "worker_cpus", "frontend_cpu", "client_cpu", "concurrency", "concurrency_multipliers", "open_connections", "rates_one", "rates_four", "queue_cap", "max_outstanding", "timeout", "headroom")},
              "topology": hardware, "sources": sources, "inputs": inputs, "artifacts": expected,
              "runtime_environment": {key: env.get(key) for key in ("BUNDLE_GEMFILE", "GEM_HOME", "GEM_PATH")},
              "host_start": host_start, "host_end": common.host(), "headroom": headroom, "summary": summary, "runs": runs, "servers": servers,
              "limits": ["local common Python HTTP/1.1 front end plus framed IPC, not a production framework comparison",
                         "closed-loop achieved capacity includes coordinated omission; fixed offered-rate windows include queue time and drops; offers late by more than one arrival interval are scheduler drops",
                         "latencies include verified full HTML/CSS body; RPS counts successful responses completed within nominal measured window",
                         "every successful response is independently hashed; failures and drained responses stay in accounting",
                         "affinity separates assigned physical cores but does not isolate host load/frequency/sibling applications",
                         "transport control caches bytes explicitly; actual renderers retain ASTs only, no response cache"]}
    write_json(marker, result)
    emit({"action": "rps_complete", "publication_valid": result["publication_valid"], "headroom": headroom, "results": str(marker)})


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--role", choices=("run", "server", "client", "control"), default="run")
    parser.add_argument("--config", type=Path)
    parser.add_argument("--report", type=Path)
    for name in ("ruby-root", "liquid-root", "theme-root", "rust-root", "rust-binary", "fixture", "expected-comparison", "output-dir", "ruby-gem-home"):
        parser.add_argument("--" + name, type=Path)
    parser.add_argument("--ruby", default="ruby")
    parser.add_argument("--ruby-gem-path")
    parser.add_argument("--duration", type=float, default=20)
    parser.add_argument("--ramp", type=float, default=2)
    parser.add_argument("--batches", type=int, default=3)
    parser.add_argument("--warmup", type=int, default=50)
    parser.add_argument("--seed", type=int, default=20261001)
    parser.add_argument("--worker-counts", type=int, nargs="+", default=[1, 4])
    parser.add_argument("--worker-cpus", type=int, nargs="+", default=[2, 3, 4, 5])
    parser.add_argument("--frontend-cpu", type=int, default=6)
    parser.add_argument("--client-cpu", type=int, default=0)
    parser.add_argument("--concurrency", type=int, nargs="+", help="Explicit connection levels for all topologies; default is workers times multipliers")
    parser.add_argument("--concurrency-multipliers", type=int, nargs="+", default=[1, 4])
    parser.add_argument("--control-concurrency", type=int, nargs="+", default=[16])
    parser.add_argument("--open-connections", type=int, default=64)
    parser.add_argument("--rates-one", type=float, nargs="+", default=[10, 40])
    parser.add_argument("--rates-four", type=float, nargs="+", default=[40, 160])
    parser.add_argument("--queue-cap", type=int, default=512)
    parser.add_argument("--max-outstanding", type=int, default=256)
    parser.add_argument("--timeout", type=float, default=5)
    parser.add_argument("--startup-timeout", type=float, default=300)
    parser.add_argument("--headroom", type=float, default=5)
    args = parser.parse_args()
    if args.role == "run":
        for name in ("ruby_root", "liquid_root", "theme_root", "rust_root", "rust_binary", "fixture", "expected_comparison", "output_dir"):
            common.require(getattr(args, name) is not None, f"missing --{name.replace('_', '-')}")
            common.require(name != "output_dir" or not getattr(args, name).is_symlink(), "output directory must not be a symlink")
            setattr(args, name, getattr(args, name).resolve())
        executable = shutil.which(args.ruby)
        args.ruby_exists = executable is not None
        args.ruby = Path(executable or args.ruby).resolve()
        args.taskset = shutil.which("taskset")
    else:
        common.require(args.config is not None and (args.role == "control" or args.report is not None), "config/report required")
    return args


def main():
    args = arguments()
    if args.role == "control":
        dummy(read_json(args.config))
    elif args.role == "server":
        asyncio.run(serve(read_json(args.config), args.report))
    elif args.role == "client":
        asyncio.run(load(read_json(args.config), args.report))
    else:
        asyncio.run(orchestrate(args))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError, asyncio.TimeoutError) as error:
        sys.exit(str(error))

"""Behavioral harness checks with tiny synthetic bodies, never real theme benchmarks."""
import asyncio
import io
import json
from pathlib import Path
import signal
import sys
import tempfile
import unittest

import rps


class Input:
    def __init__(self):
        self.written = io.BytesIO()

    def write(self, data):
        self.written.write(data)

    async def drain(self):
        pass


class MethodTest(unittest.IsolatedAsyncioTestCase):
    async def test_binary_worker_framing_and_request_id(self):
        html, css = b'<p>hello\nworld</p>', b'.x { x: "\n"; }'
        stream = asyncio.StreamReader()
        header = json.dumps(dict(request_id=9, html_bytes=len(html), css_bytes=len(css), error=None)).encode() + b'\n'
        stream.feed_data(header + html + css)
        process = type('Process', (), dict(stdin=Input(), stdout=stream))()
        worker = rps.Worker(process, {})
        expected = dict(html=rps.common.digest_bytes(html), css=rps.common.digest_bytes(css))
        self.assertEqual((html, css), await worker.render(9, expected, 1))
        self.assertEqual({'action': 'render', 'request_id': 9}, json.loads(process.stdin.written.getvalue()))
        stream.feed_data(header + html + css)
        with self.assertRaisesRegex(ValueError, 'ID differs'):
            await worker.render(8, expected, 1)

    async def test_closed_window_excludes_drain_throughput_and_retains_slow_latency(self):
        class Slow:
            async def request(self):
                await asyncio.sleep(.03)
                return None

        config = dict(request_timeout=1, max_outstanding=1)
        result = await rps.load_window(config, 'closed', .01, 1, connections=[Slow()])
        self.assertEqual(0, result['achieved_rps'])
        self.assertEqual(1, result['counts']['successful'])
        self.assertEqual(1, result['successful_during_drain'])
        self.assertGreater(result['latency']['p50_ms'], 20)

    async def test_open_offers_queue_drops_and_timeouts_are_accounted(self):
        class Slow:
            async def request(self):
                await asyncio.sleep(.1)
                return None

            async def close(self):
                pass

        config = dict(request_timeout=.02, max_outstanding=1)
        result = await rps.load_window(config, 'open', .03, 1, rate=1000, connections=[Slow()])
        self.assertEqual(30, result['counts']['offered'])
        self.assertGreater(result['counts']['queue_drop'] + result['counts']['scheduler_drop'], 0)
        self.assertGreater(result['counts']['timeout'], 0)
        self.assertEqual(result['counts']['offered'], result['counts']['started'] + result['counts']['queue_drop'] + result['counts']['scheduler_drop'])
        self.assertEqual(result['counts']['started'], result['counts']['timeout'])
        self.assertEqual(0, result['achieved_rps'])

    async def test_transport_http_keepalive_hashes_and_graceful_shutdown(self):
        with tempfile.TemporaryDirectory(prefix='horizon-rps-test-') as directory:
            root = Path(directory)
            html, css = b'<p>synthetic\npage</p>', b'.fake { color: red; }'
            (root / 'index.html').write_bytes(html)
            (root / 'styles.css').write_bytes(css)
            control = root / 'control.json'
            rps.write_json(control, dict(html_path=str(root / 'index.html'), css_path=str(root / 'styles.css'), warmup=1))
            expected = dict(html=rps.common.digest_bytes(html), css=rps.common.digest_bytes(css))
            config = dict(variant='control', commands=[[sys.executable, str(Path(rps.__file__)), '--role', 'control', '--config', str(control)]],
                          expected=expected, warmup=1, startup_timeout=5, request_timeout=1, queue_cap=2, environment={})
            server_config = root / 'server.json'
            rps.write_json(server_config, config)
            report = root / 'report.json'
            server = await asyncio.create_subprocess_exec(sys.executable, str(Path(rps.__file__)), '--role', 'server', '--config', str(server_config), '--report', str(report), stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
            try:
                line = await asyncio.wait_for(server.stdout.readline(), 5)
                if not line:
                    self.fail((await server.stderr.read()).decode())
                ready = json.loads(line)
                connection = rps.Connection(ready['port'], expected)
                self.assertIsNone(await connection.request())
                writer = connection.writer
                self.assertIsNone(await connection.request())
                self.assertIs(writer, connection.writer)
                await connection.close()
                bad = rps.Connection(ready['port'], dict(expected, html=dict(expected['html'], sha256='0' * 64)))
                with self.assertRaisesRegex(ValueError, 'digest differs'):
                    await bad.request()
                await bad.close()
            finally:
                if server.returncode is None:
                    server.send_signal(signal.SIGTERM)
                stdout, stderr = await asyncio.wait_for(server.communicate(), 5)
            self.assertEqual(0, server.returncode, stderr.decode())
            result = rps.read_json(report)
            self.assertEqual([], result['fatal'])
            self.assertEqual([0], result['worker_exit_codes'])
            self.assertEqual(3, result['counts']['sent'])
            self.assertTrue(ready['workers'][0]['ready']['response_cache'])

    def test_symlink_output_rejected_without_overwriting_target(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            target = path / 'input.json'
            target.write_text('original')
            link = path / 'report.json'
            link.symlink_to(target)
            with self.assertRaisesRegex(ValueError, 'regular output'):
                rps.write_json(link, {})
            self.assertEqual('original', target.read_text())

    async def test_timeout_kills_subprocess_descendants(self):
        with tempfile.TemporaryDirectory(prefix='horizon-rps-descendants-') as directory:
            root = Path(directory)
            pid = root / 'child.pid'
            code = "import subprocess,sys,time; from pathlib import Path; child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)']); Path(sys.argv[1]).write_text(str(child.pid)); time.sleep(60)"
            with self.assertRaisesRegex(ValueError, 'subprocess timeout'):
                await rps.subprocess_checked([sys.executable, '-c', code, str(pid)], root / 'log', .5)
            child = int(pid.read_text())
            await asyncio.sleep(.05)
            stat = Path(f'/proc/{child}/stat')
            self.assertTrue(not stat.exists() or stat.read_text().rsplit(')', 1)[1].split()[0] == 'Z')

    async def test_early_parent_exit_also_kills_subprocess_descendants(self):
        with tempfile.TemporaryDirectory(prefix='horizon-rps-descendants-') as directory:
            root = Path(directory)
            pid = root / 'child.pid'
            code = "import subprocess,sys; from pathlib import Path; child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL); Path(sys.argv[1]).write_text(str(child.pid))"
            await rps.subprocess_checked([sys.executable, '-c', code, str(pid)], root / 'log', 5)
            child = int(pid.read_text())
            await asyncio.sleep(.05)
            stat = Path(f'/proc/{child}/stat')
            self.assertTrue(not stat.exists() or stat.read_text().rsplit(')', 1)[1].split()[0] == 'Z')


if __name__ == '__main__':
    unittest.main()

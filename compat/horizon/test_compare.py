import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from compare_scenarios import execute

class OwnedProcessTest(unittest.TestCase):
    def test_timeout_kills_renderer_descendant(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            child_pid = root / 'child.pid'
            code = "import subprocess,sys,time; from pathlib import Path; child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)']);Path(sys.argv[1]).write_text(str(child.pid));time.sleep(60)"
            with self.assertRaises(subprocess.TimeoutExpired):
                execute([sys.executable, '-c', code, str(child_pid)], root / 'output', os.environ.copy(), 0.5)
            pid = int(child_pid.read_text())
            for _ in range(20):
                try:
                    state = Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()[0]
                except FileNotFoundError:
                    return
                if state == 'Z':
                    return
                time.sleep(.05)
            self.fail('owned renderer descendant remains running after timeout')

if __name__ == '__main__': unittest.main()

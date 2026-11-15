#!/usr/bin/env python3
"""Native PTY regression: Moshi must not inherit desktop SSH machine selection.

Uses an isolated HOME/server, synthetic machine catalog and loopback-only target.
No credentials, real panes or agent inference are used.

Release build: python3 scripts/test_moshi_endpoint.py --herdr target/release/herdr
Debug build: add --app-dir herdr-dev when testing target/debug/herdr.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import tempfile
import termios
import time
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--herdr', required=True)
parser.add_argument('--app-dir', choices=('herdr', 'herdr-dev'), default='herdr')
args = parser.parse_args()
binary = str(Path(args.herdr).resolve())

with tempfile.TemporaryDirectory(prefix='herdr-moshi-', dir='/tmp') as tmp:
    root = Path(tmp)
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(('HERDR_', 'CODEX_', 'CLAUDE_', 'MOSHI_', 'XDG_'))}
    env.update(HOME=tmp, XDG_CONFIG_HOME=tmp+'/.config',
               XDG_STATE_HOME=tmp+'/state', SHELL='/bin/sh', TERM='xterm-256color')
    config = root/'.config'/args.app_dir
    config.mkdir(parents=True)
    (config/'config.toml').write_text('onboarding=false\n[terminal]\ndefault_shell="/bin/sh"\n')
    state = root/'state'/args.app_dir/'client'
    state.mkdir(parents=True)
    identity = '1234567890abcdef1234567890abcdef'
    catalog = {'version': 1, 'ssh': [{'id': identity, 'label': 'PeerA',
               'target': 'ssh://127.0.0.1:9', 'session': 'default', 'enabled': True}]}
    (state/'endpoints.json').write_text(json.dumps(catalog))
    selection = state/'endpoint-selection.json'
    selection.write_text(json.dumps({'version': 1, 'selected_profile': identity}))
    original_selection = selection.read_bytes()
    session = 'moshi-test-'+uuid.uuid4().hex[:10]
    base = [binary, '--session', session]

    def command(*arguments):
        return subprocess.run(base+list(arguments), env=env, cwd=tmp,
                              capture_output=True, timeout=15)

    def attach(marker):
        child_env = dict(env)
        if marker is not None:
            child_env['MOSHI_CLIENT'] = marker
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 120, 0, 0))
        client = subprocess.Popen(base, env=child_env, cwd=tmp, stdin=slave,
                                  stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        output = bytearray()
        try:
            deadline = time.monotonic()+6
            refreshed = False
            while time.monotonic() < deadline:
                if not refreshed and time.monotonic() > deadline-3:
                    catalog['ssh'][0]['label'] = 'PeerB'
                    (state/'endpoints.json').write_text(json.dumps(catalog))
                    refreshed = True
                if select.select([master], [], [], .1)[0]:
                    data = os.read(master, 65536)
                    output.extend(data)
                    if b'\x1b[6n' in data:
                        os.write(master, b'\x1b[1;1R')
                    if b'\x1b[c' in data:
                        os.write(master, b'\x1b[?1;2c')
            assert client.poll() is None, 'fixture client exited unexpectedly'
            return re.sub(rb'\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]', b'', bytes(output))
        finally:
            client.terminate()
            client.wait(timeout=10)
            os.close(master)

    server = subprocess.Popen(base+['server'], env=env, cwd=tmp,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                              start_new_session=True)
    try:
        for _ in range(60):
            if command('workspace', 'list').returncode == 0:
                break
            time.sleep(.1)
        else:
            raise AssertionError('isolated server did not start')
        assert command('workspace', 'create', '--cwd', tmp, '--label', 'Fixture').returncode == 0
        before = json.loads(command('pane', 'list').stdout)['result']['panes']
        assert before, 'fixture has no local pane'
        assert command('pane', 'run', before[0]['pane_id'],
                       "printf 'LOCAL-MOSHI-FIXTURE\\n'").returncode == 0
        mobile = attach('1')
        assert b'LOCAL-MOSHI-FIXTURE' in mobile, 'Moshi did not render the local pane'
        assert b'PeerA' not in mobile and b'PeerB' not in mobile, \
            'Moshi loaded the desktop machine catalog at startup or reload'
        assert selection.read_bytes() == original_selection, 'Moshi rewrote desktop selection'
        ordinary = attach(None)
        assert b'PeerB' in ordinary, 'ordinary desktop client lost remote machines'
        after = json.loads(command('pane', 'list').stdout)['result']['panes']
        assert [p['pane_id'] for p in before] == [p['pane_id'] for p in after], 'panes changed'
        print('PASS Moshi startup/reload isolation, desktop federation, selection and pane preservation')
    finally:
        command('server', 'stop')
        try:
            server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            server.terminate()
            server.wait(timeout=10)

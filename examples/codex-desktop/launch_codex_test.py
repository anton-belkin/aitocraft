#!/usr/bin/env python3
"""Start/reconnect the local GridCraft Codex app and watch all of its documents."""
from runtime_paths import HUB, GRIDCRAFT_BIN
import argparse
import json
import os
from pathlib import Path
import plistlib
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.request

from runtime_paths import HERE, LAB
REGISTRY = LAB/'registry'
APP = LAB/'apps/GridCraft Codex Test.app'


def owned_pid(path, marker):
    try:
        pid = int(path.read_text().strip())
        command = subprocess.check_output(['/bin/ps', '-p', str(pid), '-o', 'command='], text=True, stderr=subprocess.DEVNULL)
        return pid if marker in command else None
    except (OSError, ValueError, subprocess.CalledProcessError):
        return None


def worker_for_instance(gui_pid):
    worker = owned_pid(LAB/'codex-bridge.pid', str(HERE/'codex_bridge.py'))
    if not worker:
        return None
    command = subprocess.check_output(['/bin/ps', '-p', str(worker), '-o', 'command='], text=True)
    worker_args = shlex.split(command.strip())
    if '--instance' in worker_args:
        index = worker_args.index('--instance')
        if index+1 < len(worker_args) and worker_args[index+1] == str(gui_pid):
            return worker
    # A previous app can exit while its worker is blocked on a dead control socket.
    # Stop that owned hub first so its parent's cleanup does not wait for a network timeout.
    processes = subprocess.check_output(['/bin/ps', '-axo', 'pid=,ppid=,command='], text=True)
    for line in processes.splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) == 3 and parts[1] == str(worker) and parts[2] == str(HUB):
            try:
                os.kill(int(parts[0]), signal.SIGTERM)
            except ProcessLookupError:
                pass
    try:
        os.kill(worker, signal.SIGTERM)
    except ProcessLookupError:
        return None
    for _ in range(50):
        if owned_pid(LAB/'codex-bridge.pid', str(HERE/'codex_bridge.py')) != worker:
            return None
        time.sleep(0.1)
    if owned_pid(LAB/'codex-bridge.pid', str(HERE/'codex_bridge.py')) == worker:
        os.kill(worker, signal.SIGKILL)
    return None


def prepare_app(raw):
    macos = APP/'Contents/MacOS'
    macos.mkdir(parents=True, exist_ok=True)
    executable = macos/'gridcraft-bin'
    if not executable.exists() or executable.stat().st_mtime < raw.stat().st_mtime:
        shutil.copy2(raw, executable)
        executable.chmod(0o755)
    # Finder/Dock launches must start the worker too, not just the editor binary.
    wrapper = macos/'GridCraft'
    wrapper.write_text('#!/bin/bash\nexport GRIDCRAFT_CODEX_STATE_DIR='+shlex.quote(str(LAB))+
                       '\nexec /usr/bin/python3 '+shlex.quote(str(HERE/'launch_codex_test.py'))+' "$@"\n')
    wrapper.chmod(0o755)
    info = {'CFBundleName':'GridCraft Codex Test', 'CFBundleDisplayName':'GridCraft Codex Test',
        'CFBundleIdentifier':'org.aitocraft.gridcraft-codex-test', 'CFBundleExecutable':'GridCraft',
        'CFBundlePackageType':'APPL', 'CFBundleVersion':'0.7.3', 'CFBundleShortVersionString':'0.7.3',
        'NSHighResolutionCapable':True,
        'NSMicrophoneUsageDescription':'Optional voice questions in GridCraft.'}
    with (APP/'Contents/Info.plist').open('wb') as f:
        plistlib.dump(info, f)
    return executable


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--file', type=Path, action='append', default=[])
    parser.add_argument('files', type=Path, nargs='*')
    parser.add_argument('--no-worker', action='store_true')
    args = parser.parse_args()
    raw = GRIDCRAFT_BIN
    hub = HUB
    if not raw.is_file() or not hub.is_file():
        raise RuntimeError('Соберите оба бинарника и выполните setup.py; см. README.md.')
    files = [p.resolve() for p in args.file+args.files]
    gui_pid = owned_pid(LAB/'gridcraft-test.pid', str(APP/'Contents/MacOS'))
    if gui_pid is None:
        for port in (7979, 7980):
            with socket.socket() as s:
                s.settimeout(0.2)
                if s.connect_ex(('127.0.0.1', port)) == 0:
                    raise RuntimeError(f'Порт {port} занят другим процессом. Подключение к нему не выполнялось.')
        executable = prepare_app(raw)
        for p in files:
            if not p.is_file():
                raise RuntimeError('Нет файла: '+str(p))
        logs = LAB/'logs'
        logs.mkdir(parents=True, exist_ok=True)
        REGISTRY.mkdir(parents=True, exist_ok=True)
        env = dict(os.environ, AITOCRAFT_BIN=str(hub), AITOCRAFT_REGISTRY_DIR=str(REGISTRY),
            CRAFT_VOICE_MODELS=str(LAB/'voice-models'), GRIDCRAFT_NO_PREFS='1',
            GRIDCRAFT_AGENT_PROVIDER='codex', GRIDCRAFT_CODEX_BRIDGE=str(HERE/'codex_bridge.py'), GRIDCRAFT_AGENT_COMMAND='aitocraft-codex-external-worker-only')
        with (logs/'gridcraft-codex-ui.log').open('a') as log:
            gui = subprocess.Popen([str(executable), '--agent-mode', 'connect', '--agent-port', '7980',
                '--control', '7979']+([str(files[0])] if files else []), stdout=log, stderr=log, env=env, start_new_session=True)
        gui_pid = gui.pid
        (LAB/'gridcraft-test.pid').write_text(str(gui_pid)+'\n')
        for attempt in range(60):
            if gui.poll() is not None:
                raise RuntimeError(f'GridCraft завершился с кодом {gui.returncode}; см. logs/gridcraft-codex-ui.log')
            try:
                with urllib.request.urlopen('http://127.0.0.1:7980/health', timeout=1) as response:
                    if response.read().strip() == b'ok':
                        break
            except Exception:
                pass
            time.sleep(0.5)
        else:
            raise RuntimeError('GridCraft не ответил /health за 30 секунд')
        files = files[1:]
    from gridcraft_control import call
    for path in files:
        call('app.open', {'path':str(path)})
    call('ui.focus')
    from gridcraft_links import ensure_server
    try:
        ensure_server()
    except (OSError, RuntimeError) as error:
        print('Ссылки недоступны: '+str(error), file=sys.stderr)
    print('GridCraft Codex открыт. Новые книги подключаются автоматически.', flush=True)
    if args.no_worker:
        return 0
    worker = worker_for_instance(gui_pid)
    if worker:
        print('Codex уже подключён, PID '+str(worker), flush=True)
        return 0
    for attempt in range(40):
        if (REGISTRY/f'gridcraft-{gui_pid}.json').is_file():
            break
        time.sleep(0.25)
    print('Codex: запуск обработчика всех книг. Ctrl+C остановит обработчик.', flush=True)
    os.execv(sys.executable, [sys.executable, str(HERE/'codex_bridge.py'), '--instance', str(gui_pid), '--registry', str(REGISTRY)])


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception as exc:
        print('Ошибка:', exc, file=sys.stderr)
        sys.exit(2)

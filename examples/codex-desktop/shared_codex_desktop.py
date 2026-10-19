#!/usr/bin/env python3
"""Experimental shared-server launch; no persistent Codex or Claude configuration changes.

Uses the installed Desktop's WebSocket transport for the GridCraft chat bridge.
Refuses to relaunch over a running Desktop or reuse an unknown listener.
"""
from runtime_paths import HUB, NODE
import argparse
import json
import os
from pathlib import Path
import plistlib
import socket
import subprocess
import sys
import time
import urllib.request
import uuid

from runtime_paths import HERE, LAB
from runtime_paths import DESKTOP_APP
APP = DESKTOP_APP
CLI = APP / 'Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex'
URL = 'ws://127.0.0.1:45123'


def desktop_executable():
    with (APP / 'Contents/Info.plist').open('rb') as stream:
        info = plistlib.load(stream)
    return APP / 'Contents/MacOS' / info['CFBundleExecutable']


def desktop_pids(executable):
    listing = subprocess.check_output(['/bin/ps', '-axo', 'pid=,comm='], text=True)
    return [int(parts[0]) for line in listing.splitlines()
            if len(parts := line.strip().split(None, 1)) == 2 and parts[1] == str(executable)]


def occupied():
    with socket.socket() as connection:
        connection.settimeout(.3)
        return connection.connect_ex(('127.0.0.1', 45123)) == 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--launch', action='store_true', help='Start only after Desktop has been closed')
    parser.add_argument('--thread', help='Existing chat to open; defaults to the last launcher selection, never used to issue links')
    args = parser.parse_args()
    previous = LAB/'codex-desktop-runtime.json'
    selected = args.thread or (json.loads(previous.read_text()).get('originThread') if previous.exists() else None)
    thread = str(uuid.UUID(selected)) if selected else None
    executable = desktop_executable()
    for required in [executable, CLI, Path(NODE), HERE / 'app_server_probe.mjs']:
        if not required.is_file():
            raise RuntimeError('Отсутствует файл: ' + str(required))
    pids = desktop_pids(executable)
    busy = occupied()
    plan = {
        'endpoint': URL, 'originThread': thread, 'desktopPids': pids,
        'portOccupied': busy, 'persistentSettingsChanged': False,
        'launchEnvironment': {'CODEX_APP_SERVER_WS_URL': URL},
        'requiresRuntimeCheck': True, 'gridcraftRoutingImplemented': True,
    }
    print(json.dumps(plan, ensure_ascii=False, indent=2), flush=True)
    if not args.launch:
        return 0
    if pids:
        raise RuntimeError('Сначала завершите Codex/ChatGPT Desktop через Cmd+Q, затем повторно запустите этот файл. '
                           'Текущие чаты и процессы скрипт не завершает.')
    if busy:
        raise RuntimeError('Порт 45123 уже занят. Скрипт не подключается к неизвестному серверу.')
    hub = HUB
    if not hub.is_file():
        raise RuntimeError('Сначала соберите AItoCraft: ' + str(hub))
    logs = LAB / 'logs'
    logs.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    # Do not carry the calling agent's transient identity or private tool pipe into a new server.
    for key in ['CODEX_THREAD_ID', 'CODEX_APP_TOOLS_PIPE_PATH', 'CODEX_APP_SERVER_WS_URL',
                'CODEX_APP_SERVER_FORCE_CLI']:
        env.pop(key, None)
    command = [str(CLI), 'app-server', '--listen', URL,
               '-c', 'features.code_mode_host=true',
               '-c', 'mcp_servers.aitocraft.command=' + json.dumps(str(hub)),
               '-c', 'mcp_servers.aitocraft.env={AITOCRAFT_REGISTRY_DIR=' + json.dumps(str(LAB / 'registry')) + ',AITOCRAFT_CHANNELS="0"}',
               '-c', 'mcp_servers.aitocraft.enabled=true']
    server = None
    desktop = None
    try:
        with (logs / 'codex-shared-server.log').open('a') as server_log, \
             (logs / 'codex-shared-desktop.log').open('a') as desktop_log:
            server = subprocess.Popen(command, cwd=LAB, env=env, stdout=server_log, stderr=server_log)
            for _ in range(100):
                if server.poll() is not None:
                    raise RuntimeError('App Server завершился. Лог: ' + str(logs / 'codex-shared-server.log'))
                try:
                    with urllib.request.urlopen('http://127.0.0.1:45123/readyz', timeout=.3) as response:
                        if response.status == 200:
                            break
                except OSError:
                    time.sleep(.1)
            else:
                raise RuntimeError('App Server не ответил /readyz за 10 секунд')
            # Prove the public endpoint accepts both clients before opening Desktop.
            subprocess.run([str(NODE), str(HERE / 'app_server_probe.mjs'), '--url', URL],
                           check=True, timeout=35, env=env)
            desktop_env = dict(env, CODEX_APP_SERVER_WS_URL=URL)
            desktop = subprocess.Popen([str(executable)]+(['codex://threads/' + thread] if thread else []),
                                       env=desktop_env, stdout=desktop_log, stderr=desktop_log)
            runtime = LAB / 'codex-desktop-runtime.json'
            runtime.write_text(json.dumps({'endpoint': URL, 'desktopPid': desktop.pid,
                'serverPid': server.pid, 'originThread': thread}, indent=2)+'\n')
            print('Desktop запущен с общим App Server. Откройте нужный чат, '
                  'затем запустите Start GridCraft.command.', flush=True)
            print('GridCraft доставляет вопросы через этот общий сервер. '
                  'Оставьте это окно открытым, пока работаете с Desktop.', flush=True)
            return desktop.wait()
    finally:
        # Closing the launcher while Desktop uses this endpoint must not cut off its chats.
        if server is not None and (desktop is None or desktop.poll() is not None):
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=5)


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception as error:
        print('Ошибка:', error, file=sys.stderr)
        sys.exit(2)

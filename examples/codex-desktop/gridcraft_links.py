#!/usr/bin/env python3
"""Local GridCraft links: opaque tickets, explicit chat bindings and a loopback landing page."""
from runtime_paths import HUB
import argparse
import contextlib
import fcntl
import html
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import sys
import time
from urllib.parse import urlsplit
import uuid
import zipfile

from runtime_paths import HERE, LAB
STATE = LAB / 'run/gridcraft-links'
PORT = 45124
TOKEN = re.compile(r'[a-f0-9]{48}')


def installation_id():
    return hashlib.sha256(str(LAB).encode()).hexdigest()


def atomic_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + '.' + secrets.token_hex(8) + '.tmp')
    fd = os.open(str(tmp), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w') as out:
        json.dump(value, out, ensure_ascii=False, indent=2)
        out.write('\n')
    tmp.replace(path)


def valid_file(path):
    path = Path(path).expanduser().resolve(strict=True)
    if not path.is_file() or path.suffix.lower() != '.xlsx' or not zipfile.is_zipfile(path):
        raise ValueError('Нужен существующий локальный файл .xlsx.')
    return path


def origin_from_file(path):
    # Bounded read of our own metadata; workbook content never becomes instructions.
    with zipfile.ZipFile(path) as archive:
        try:
            info = archive.getinfo('customXml/gridcraftAgent.xml')
        except KeyError:
            return {}
        if info.file_size > 4 * 1024 * 1024:
            raise ValueError('Слишком большой слой Agent.')
        text = archive.read(info).decode('utf-8')
    match = re.search(r'<!\[CDATA\[(.*?)\]\]>', text, re.S)
    context = json.loads(match.group(1)).get('context', {}) if match else {}
    return {'threadId': context.get('codex_thread_id', ''), 'title': context.get('codex_thread_title', '')}


class LinkStore:
    def __init__(self, root=STATE):
        self.root = Path(root)

    def issue(self, path, thread, title, cwd, lifetime=30*86400):
        path = valid_file(path)
        thread = str(uuid.UUID(thread))
        if lifetime <= 0:
            raise ValueError('Срок действия должен быть положительным.')
        token = secrets.token_hex(24)
        ticket = {'version': 1, 'path': str(path), 'threadId': thread,
                  'title': title[:160], 'cwd': str(Path(cwd).resolve()),
                  'createdAt': time.time(), 'expiresAt': time.time()+lifetime}
        atomic_json(self.root / 'tickets' / (token+'.json'), ticket)
        return token, ticket

    def read(self, token):
        if not TOKEN.fullmatch(token):
            raise ValueError('Некорректная ссылка GridCraft.')
        path = self.root / 'tickets' / (token+'.json')
        if not path.is_file() or path.stat().st_size > 16384:
            raise ValueError('Локальная ссылка не найдена.')
        ticket = json.loads(path.read_text())
        if ticket.get('version') != 1 or ticket['expiresAt'] < time.time():
            raise ValueError('Срок действия ссылки истёк. Создайте новую ссылку из нужного чата.')
        valid_file(ticket['path'])
        uuid.UUID(ticket['threadId'])
        return ticket

    @contextlib.contextmanager
    def bindings(self):
        self.root.mkdir(parents=True, exist_ok=True)
        with (self.root/'bindings.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            path = self.root/'bindings.json'
            records = json.loads(path.read_text()) if path.exists() else {}
            before = json.dumps(records, sort_keys=True)
            yield records
            if json.dumps(records, sort_keys=True) != before:
                atomic_json(path, records)

    def bind(self, token, ticket, document=None):
        with self.bindings() as records:
            records[ticket['path']] = dict(ticket, token=token, documentId=document, boundAt=time.time())

    def for_document(self, doc):
        with self.bindings() as records:
            # The current path wins; document IDs change on reopen and can be reused after process restart.
            value = records.get(doc.get('path'))
            if value is None:
                candidates = [v for v in records.values() if v.get('documentId') == doc['id']]
                value = max(candidates, key=lambda v: v['boundAt']) if candidates else None
            if value is None:
                return None
            value = dict(value)
            value['documentId'] = doc['id']
            if doc.get('path'):
                records[doc['path']] = value
            return value


def token_from_url(url):
    parsed = urlsplit(url)
    if (parsed.scheme != 'gridcraft' or parsed.netloc != 'open' or parsed.query or parsed.fragment
            or not TOKEN.fullmatch(parsed.path.removeprefix('/'))):
        raise ValueError('Ожидается gridcraft://open/<локальный ticket>.')
    return parsed.path[1:]


def target_status(thread):
    from desktop_bridge import DesktopRPC, loaded_threads, runtime_config
    try:
        config = runtime_config()
        with (LAB/'logs/link-rpc.log').open('a') as err:
            rpc = DesktopRPC(config['endpoint'], err)
            try:
                loaded = loaded_threads(rpc)
                metadata = rpc.rpc('thread/read', {'threadId': thread, 'includeTurns': False})['thread']
                return {'available': thread in loaded, 'title': metadata.get('name') or metadata.get('preview') or 'Codex Desktop',
                        'cwd': metadata.get('cwd') or str(Path.cwd())}
            finally:
                rpc.close()
    except (OSError, RuntimeError, ValueError, KeyError) as exc:
        return {'available': False, 'reason': str(exc)}


def open_ticket(token):
    STATE.mkdir(parents=True, exist_ok=True)
    with (STATE/'open.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        return _open_ticket(token)


def _open_ticket(token):
    from gridcraft_control import call
    from launch_codex_test import APP, owned_pid
    store = LinkStore()
    ticket = store.read(token)
    # Persist before launching: the question listener must see the explicit route on its first poll.
    store.bind(token, ticket)
    command = ['/usr/bin/python3', str(HERE/'launch_codex_test.py'), '--no-worker']
    if not owned_pid(LAB/'gridcraft-test.pid', str(APP/'Contents/MacOS')):
        command += ['--file', ticket['path']]
    result = subprocess.run(command,
                            capture_output=True, text=True, timeout=45)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip())
    docs = call('engine.execute', {'command': 'agent.listDocuments', 'params': {}})
    doc = next((d for d in docs if d.get('path') == ticket['path']), None)
    if doc is None:
        call('app.open', {'path': ticket['path']})
        docs = call('engine.execute', {'command': 'agent.listDocuments', 'params': {}})
        doc = next(d for d in docs if d.get('path') == ticket['path'])
    else:
        call('engine.execute', {'command': 'window.activate', 'params': {'index': docs.index(doc)}})
    store.bind(token, ticket, doc['id'])
    status = target_status(ticket['threadId'])
    call('engine.execute', {'command': 'agent.onDocument', 'params': {'document': doc['id'],
        'command': 'agent.codexRoute', 'params': {
            'status': 'linked' if status['available'] else 'disconnected',
            'threadId': ticket['threadId'], 'title': ticket['title'],
            'detail': 'Чат выбран ссылкой. Происхождение файла сохранено.' if status['available'] else
                      'Чат выбран ссылкой; общий App Server Desktop сейчас недоступен или чат не загружен.'}}})
    gui = owned_pid(LAB/'gridcraft-test.pid', str(APP/'Contents/MacOS'))
    # Launcher may become the long-lived worker; it is intentionally detached from the link handler.
    with (LAB/'logs/link-launch.log').open('a') as log:
        subprocess.Popen(['/usr/bin/python3', str(HERE/'launch_codex_test.py')], stdout=log, stderr=log,
                         start_new_session=True)
    call('ui.focus')
    record = {'document': doc['id'], 'path': ticket['path'], 'threadId': ticket['threadId'],
              'title': ticket['title'], 'targetAvailable': status['available'], 'guiPid': gui, 'at': time.time()}
    atomic_json(STATE/'last-open.json', record)
    return record


def ensure_server():
    import urllib.request
    try:
        with urllib.request.urlopen(f'http://127.0.0.1:{PORT}/health', timeout=1) as response:
            health = json.load(response)
            if health.get('service') == 'gridcraft-links-v1' and health.get('installationId') == installation_id():
                return
        raise RuntimeError('Порт ссылок занят другой установкой или приложением. Остановите прежний сервис ссылок.')
    except OSError:
        pass
    (LAB/'logs').mkdir(parents=True, exist_ok=True)
    with (LAB/'logs/gridcraft-links.log').open('a') as log:
        subprocess.Popen(['/usr/bin/python3', str(HERE/'gridcraft_links.py'), 'serve'],
                         stdout=log, stderr=log, start_new_session=True)
    for _ in range(40):
        try:
            with urllib.request.urlopen(f'http://127.0.0.1:{PORT}/health', timeout=.5) as response:
                health = json.load(response)
                if health.get('service') == 'gridcraft-links-v1' and health.get('installationId') == installation_id():
                    return
        except OSError:
            time.sleep(.1)
    raise RuntimeError('Не удалось запустить локальный обработчик ссылок.')


def serve():
    origin = f'http://127.0.0.1:{PORT}'
    class Handler(BaseHTTPRequestHandler):
        def reply(self, code, body, content='text/html; charset=utf-8'):
            self.send_response(code)
            self.send_header('Content-Type', content)
            self.send_header('Cache-Control', 'no-store')
            self.send_header('Content-Security-Policy', "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'")
            self.send_header('X-Content-Type-Options', 'nosniff')
            self.end_headers()
            self.wfile.write(body.encode())

        def do_GET(self):
            if self.headers.get('Host') != f'127.0.0.1:{PORT}':
                return self.reply(403, 'Localhost only')
            if self.path == '/health':
                return self.reply(200, json.dumps({'service': 'gridcraft-links-v1', 'installationId': installation_id()}), 'application/json')
            try:
                if not self.path.startswith('/open/'):
                    raise ValueError('Ссылка не найдена.')
                token = self.path[len('/open/'):]
                ticket = LinkStore().read(token)
                # GET is read-only: link previews and prefetch must not open files or change routing.
                body = ('<!doctype html><meta charset="utf-8"><title>Открыть в GridCraft</title>'
                    '<style>body{font:18px system-ui;max-width:650px;margin:12vh auto;padding:24px;line-height:1.6}'
                    'button{font:inherit;padding:12px 20px;cursor:pointer}small{overflow-wrap:anywhere}</style>'
                    '<h1>Открыть в GridCraft</h1><p>'+html.escape(Path(ticket['path']).name)+'</p>'
                    '<p>Чат: <strong>'+html.escape(ticket['title'])+'</strong></p><small>'+html.escape(ticket['path'])+'</small>'
                    '<form method="post"><p><button>Открыть файл и подключить этот чат</button></p></form>'
                    '<p><small>Экспериментальная локальная ссылка. Работает на этом Mac.</small></p>')
                self.reply(200, body)
            except (OSError, ValueError, KeyError) as exc:
                self.reply(404, html.escape(str(exc)))

        def do_POST(self):
            if (self.headers.get('Host') != f'127.0.0.1:{PORT}' or self.headers.get('Origin') != origin
                    or not self.path.startswith('/open/')):
                return self.reply(403, 'Local origin required')
            try:
                result = open_ticket(self.path[len('/open/'):])
                self.reply(200, '<meta charset="utf-8"><title>GridCraft</title><h1>Файл открыт в GridCraft</h1><p>Чат: '+
                           html.escape(result['title'])+'</p>'+('' if result['targetAvailable'] else
                           '<p>Для ответов запустите Desktop через launcher общего App Server.</p>'))
            except Exception as exc:
                self.reply(400, html.escape(str(exc)))
    server = ThreadingHTTPServer(('127.0.0.1', PORT), Handler)
    (STATE).mkdir(parents=True, exist_ok=True)
    (STATE/'server.pid').write_text(str(os.getpid()))
    server.serve_forever()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    issue = sub.add_parser('issue')
    issue.add_argument('path', type=Path)
    issue.add_argument('--thread', help='Exact chat ID; default is CODEX_THREAD_ID')
    issue.add_argument('--chat', choices=['current', 'origin'], default='current')
    issue.add_argument('--title')
    issue.add_argument('--days', type=int, default=30)
    sub.add_parser('serve')
    opening = sub.add_parser('open')
    opening.add_argument('url')
    mcp = sub.add_parser('mcp', help='Call AItoCraft transport in this chat; does not start an agent')
    mcp.add_argument('tool')
    mcp.add_argument('--args', default='{}')
    schema = sub.add_parser('tools', help='Inspect AItoCraft tool schemas without starting an agent')
    schema.add_argument('name', nargs='?')
    sub.add_parser('status')
    args = parser.parse_args()
    if args.command == 'serve':
        return serve()
    if args.command == 'open':
        print(json.dumps(open_ticket(token_from_url(args.url)), ensure_ascii=False))
    elif args.command == 'issue':
        origin = origin_from_file(valid_file(args.path)) if args.chat == 'origin' else {}
        thread = args.thread or (origin.get('threadId') if args.chat == 'origin' else os.environ.get('CODEX_THREAD_ID'))
        if not thread:
            raise ValueError('ID чата неизвестен. Нужен CODEX_THREAD_ID, --thread либо сохранённый origin; чужой чат автоматически не выбирается.')
        thread = str(uuid.UUID(thread))
        status = target_status(thread)
        title = args.title or status.get('title') or origin.get('title') or 'Codex Desktop · '+thread[:8]
        token, ticket = LinkStore().issue(args.path, thread, title, status.get('cwd', Path.cwd()), args.days*86400)
        ensure_server()
        print(json.dumps({'url': f'http://127.0.0.1:{PORT}/open/{token}', 'deepLink': f'gridcraft://open/{token}',
                          'path': ticket['path'], 'threadId': thread, 'title': title, 'targetAvailable': status['available'],
                          'reason': status.get('reason'), 'expiresAt': ticket['expiresAt']}, ensure_ascii=False, indent=2))
    elif args.command in ('mcp', 'tools'):
        from codex_bridge import Hub
        with (LAB/'logs/link-mcp.log').open('a') as err:
            hub = Hub(HUB, LAB/'registry', err, client_name='Codex skill transport')
            try:
                if args.command == 'tools':
                    tools = hub.rpc('tools/list', {})['tools']
                    value = next((tool for tool in tools if tool['name'] == args.name), None) if args.name else [tool['name'] for tool in tools]
                else:
                    value = hub.call(args.tool, json.loads(args.args))
                print(json.dumps(value, ensure_ascii=False, indent=2))
            finally:
                hub.close()
    else:
        print(json.dumps(target_status(os.environ.get('CODEX_THREAD_ID', '')), ensure_ascii=False, indent=2))


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print('Ошибка: '+str(error), file=sys.stderr)
        sys.exit(2)

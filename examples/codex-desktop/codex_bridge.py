#!/usr/bin/env python3
"""AItoCraft transport helpers and entry point for the shared Codex Desktop bridge."""
import datetime
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time

from runtime_paths import HERE, LAB


class Hub:
    def __init__(self, executable, registry, err_file, client_name="Codex local bridge"):
        env = dict(os.environ, AITOCRAFT_REGISTRY_DIR=str(registry), AITOCRAFT_CHANNELS='0')
        self.p = subprocess.Popen([str(executable)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=err_file, text=True, encoding='utf-8', env=env)
        self.queue = queue.Queue()
        self.seq = 0
        def read():
            try:
                for line in self.p.stdout:
                    self.queue.put(json.loads(line))
            except Exception as exc:
                self.queue.put(exc)
            finally:
                self.queue.put(EOFError('AItoCraft exited'))
        threading.Thread(target=read, daemon=True).start()
        self.rpc('initialize', {'protocolVersion':'2025-06-18', 'capabilities':{},
                               'clientInfo':{'name':client_name, 'version':'1'}})
        self.send({'jsonrpc':'2.0','method':'notifications/initialized'})

    def send(self, value):
        self.p.stdin.write(json.dumps(value, ensure_ascii=False)+'\n')
        self.p.stdin.flush()

    def rpc(self, method, params, timeout=75):
        self.seq += 1
        self.send({'jsonrpc':'2.0','id':self.seq,'method':method,'params':params})
        end = time.monotonic()+timeout
        while True:
            try:
                value = self.queue.get(timeout=max(0,end-time.monotonic()))
            except queue.Empty as exc:
                raise RuntimeError('MCP timeout: '+method) from exc
            if isinstance(value, Exception):
                raise value
            if value.get('id') != self.seq:
                continue
            if 'error' in value:
                raise RuntimeError(str(value['error']))
            return value['result']

    def call(self, name, arguments=None):
        result = self.rpc('tools/call', {'name':name,'arguments':arguments or {}})
        if result.get('isError'):
            raise RuntimeError(json.dumps(result, ensure_ascii=False))
        if 'structuredContent' in result:
            return result['structuredContent']
        for c in result.get('content',[]):
            if c.get('type')=='text':
                try:
                    return json.loads(c['text'])
                except json.JSONDecodeError:
                    continue
        return result

    def close(self):
        if self.p.poll() is None:
            self.p.stdin.close()
            try:
                self.p.wait(timeout=4)
            except subprocess.TimeoutExpired:
                self.p.terminate()
                self.p.wait(timeout=4)


def event(kind, **fields):
    value = dict(at=datetime.datetime.now().astimezone().isoformat(), event=kind, **fields)
    with (LAB/'logs/codex-bridge-events.jsonl').open('a') as f:
        f.write(json.dumps(value, ensure_ascii=False)+'\n')
    print(kind, ' '.join(f'{k}={v}' for k,v in fields.items()), flush=True)


def matches_instance(doc, instance):
    return doc.get('app') == 'gridcraft' and (not instance or doc.get('id', '').startswith(f'gridcraft:{instance}:'))


def questions_list(result):
    return result.get('questions', []) if isinstance(result, dict) else result


if __name__ == '__main__':
    from desktop_bridge import main
    sys.exit(main())

#!/usr/bin/env python3
"""Create a workbook from a Codex chat and save its origin in the workbook's agent layer."""
import argparse
import json
import os
from pathlib import Path
import uuid

from gridcraft_control import call


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('path', type=Path)
    parser.add_argument('--thread', default=os.environ.get('CODEX_THREAD_ID'))
    parser.add_argument('--title', default='Codex Desktop')
    args = parser.parse_args()
    if not args.thread:
        raise RuntimeError('Запустите из чата Codex или передайте --thread с его техническим ID.')
    thread = str(uuid.UUID(args.thread))
    path = args.path.resolve()
    if path.suffix.lower() != '.xlsx' or path.exists():
        raise RuntimeError('Укажите новый файл .xlsx; существующие файлы не перезаписываются.')
    path.parent.mkdir(parents=True, exist_ok=True)
    def engine(command, params=None):
        return call('engine.execute', {'command': command, 'params': params or {}})
    engine('file.new')
    document = next(d['id'] for d in engine('agent.listDocuments') if d['frontmost'])
    def on_document(command, params):
        return engine('agent.onDocument', {'document': document, 'command': command, 'params': params})
    on_document('agent.codexOrigin', {'threadId': thread, 'title': args.title})
    on_document('file.save', {'path': str(path)})
    print(json.dumps({'document': document, 'path': str(path), 'originThread': thread}, ensure_ascii=False))


if __name__ == '__main__':
    main()

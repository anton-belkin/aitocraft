#!/usr/bin/env python3
"""Stable global skill entry point; runtime location is installation data, independent of cwd."""
import json
import os
from pathlib import Path
import sys

skill = Path(__file__).resolve().parents[1]
config = json.loads((skill/'runtime.json').read_text())
helper = Path(config['helper'])
if config.get('stateDir'):
    os.environ['GRIDCRAFT_CODEX_STATE_DIR'] = config['stateDir']
if not helper.is_file():
    sys.exit('GridCraft runtime не найден. Повторите setup.py --install-skill --replace-skill из examples/codex-desktop.')
os.execv('/usr/bin/python3', ['/usr/bin/python3', str(helper), *sys.argv[1:]])

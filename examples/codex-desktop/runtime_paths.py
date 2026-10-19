"""Installation paths for the experimental macOS example; mutable state stays outside the repo."""
import json
import os
from pathlib import Path
import shutil

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
LAB = Path(os.environ.get('GRIDCRAFT_CODEX_STATE_DIR',
           str(Path.home()/'Library/Application Support/AItoCraft/Codex Desktop'))).expanduser().resolve()


def read_config(root=LAB):
    path = Path(root)/'installation.json'
    if path.exists():
        return json.loads(path.read_text())
    desktop = Path('/Applications/ChatGPT.app')
    bundled_node = desktop/'Contents/Resources/cua_node/bin/node'
    return {'aitocraft': str(REPO/'target/release/aitocraft'),
            'gridcraft': str(REPO.parent/'gridcraft/target/release/gridcraft'),
            'desktopApp': str(desktop), 'node': str(bundled_node) if bundled_node.is_file() else (shutil.which('node') or 'node'),
            'workspace': str(Path(root)/'workspace')}


CONFIG = read_config()
HUB = Path(CONFIG['aitocraft']).expanduser().resolve()
GRIDCRAFT_BIN = Path(CONFIG['gridcraft']).expanduser().resolve()
DESKTOP_APP = Path(CONFIG['desktopApp']).expanduser().resolve()
NODE = CONFIG['node']
DEFAULT_WORKSPACE = Path(CONFIG['workspace']).expanduser().resolve()

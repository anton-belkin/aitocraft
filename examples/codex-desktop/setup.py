#!/usr/bin/env python3
"""Configure built binaries and generate local launchers; does not start Desktop or install login items."""
import argparse
import json
import os
from pathlib import Path
import plistlib
import shlex
import shutil
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent


def setup(args):
    root = args.state_dir.expanduser().resolve()
    root.mkdir(parents=True, exist_ok=True)
    desktop = args.desktop_app.expanduser().resolve()
    with (desktop/'Contents/Info.plist').open('rb') as stream:
        info = plistlib.load(stream)
    required = [args.aitocraft.resolve(), args.gridcraft.resolve(), desktop/'Contents/MacOS'/info['CFBundleExecutable'],
                desktop/'Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex']
    for path in required:
        if not path.is_file() or not os.access(path, os.X_OK):
            raise ValueError('Build/install the executable first: '+str(path))
    bundled_node = desktop/'Contents/Resources/cua_node/bin/node'
    node = args.node or (str(bundled_node) if bundled_node.is_file() else shutil.which('node'))
    if not node:
        raise ValueError('Node 22+ is required; pass --node /absolute/path/to/node.')
    node = shutil.which(node) or node
    subprocess.run([node, '--input-type=module', '-e',
                    "if (Number(process.versions.node.split('.')[0]) < 22 || typeof WebSocket !== 'function') process.exit(1)"], check=True)
    config = {'aitocraft': str(required[0]), 'gridcraft': str(required[1]), 'desktopApp': str(desktop),
              'node': str(Path(node).resolve()), 'workspace': str(root/'workspace')}
    for name in ['logs', 'registry', 'run', 'workspace']:
        (root/name).mkdir(exist_ok=True)
    (root/'installation.json').write_text(json.dumps(config, indent=2)+'\n')
    for filename, script in [('Start Codex Desktop.command','shared_codex_desktop.py'),
                             ('Start GridCraft.command','launch_codex_test.py')]:
        launch = root/filename
        argument = ' --launch' if script == 'shared_codex_desktop.py' else ''
        launch.write_text('#!/bin/bash\nexport GRIDCRAFT_CODEX_STATE_DIR='+shlex.quote(str(root))+
                          '\nexec /usr/bin/python3 '+shlex.quote(str(HERE/script))+argument+' "$@"\n')
        launch.chmod(0o755)
    if not args.no_register_links:
        env = dict(os.environ, GRIDCRAFT_CODEX_STATE_DIR=str(root))
        subprocess.run(['/usr/bin/python3', str(HERE/'install_gridcraft_links.py')], env=env, check=True)
    if args.install_skill:
        destination = (args.skill_dir.expanduser().resolve() if args.skill_dir else
                       Path(os.environ.get('CODEX_HOME', str(Path.home()/'.codex')))/'skills/gridcraft-chat-experimental')
        if destination.exists():
            marker = destination/'runtime.json'
            if not args.replace_skill or not marker.is_file() or not json.loads(marker.read_text()).get('experimental'):
                raise ValueError('Skill already exists; use --replace-skill only for an existing experimental installation.')
            backup = root/'skill-backups'/('gridcraft-chat-experimental-'+str(time.time_ns()))
            backup.parent.mkdir(parents=True, exist_ok=True)
            destination.rename(backup)
        shutil.copytree(HERE/'skill/gridcraft-chat-experimental', destination)
        (destination/'runtime.json').write_text(json.dumps({'helper': str(HERE/'gridcraft_links.py'),
            'stateDir': str(root), 'experimental': True}, indent=2)+'\n')
        print('Skill: '+str(destination))
    print('Installation: '+str(root/'installation.json'))
    print('Launch after reboot: '+str(root/'Start Codex Desktop.command'))
    print('Then: '+str(root/'Start GridCraft.command'))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--aitocraft', type=Path, default=HERE.parents[1]/'target/release/aitocraft')
    parser.add_argument('--gridcraft', type=Path, required=True)
    parser.add_argument('--desktop-app', type=Path, default=Path('/Applications/ChatGPT.app'))
    parser.add_argument('--node')
    parser.add_argument('--state-dir', type=Path, default=Path.home()/'Library/Application Support/AItoCraft/Codex Desktop')
    parser.add_argument('--install-skill', action='store_true')
    parser.add_argument('--skill-dir', type=Path, help='Explicit skill destination, useful for isolated installation verification')
    parser.add_argument('--replace-skill', action='store_true')
    parser.add_argument('--no-register-links', action='store_true', help='Configure paths without compiling/registering the macOS URL receiver')
    setup(parser.parse_args())


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))

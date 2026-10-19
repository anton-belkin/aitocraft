"""Installation contracts: another cwd, paths with spaces, opt-in skill replacement, no auto-start."""
import argparse
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import setup


class InstallationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='GridCraft setup ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.desktop = self.root/'ChatGPT Test.app'
        (self.desktop/'Contents').mkdir(parents=True)
        with (self.desktop/'Contents/Info.plist').open('wb') as out:
            plistlib.dump({'CFBundleExecutable': 'Desktop'}, out)
        self.files = [self.root/'built apps/aitocraft', self.root/'built apps/gridcraft',
                      self.desktop/'Contents/MacOS/Desktop',
                      self.desktop/'Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex']
        for file in self.files:
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_text('fixture, not executed')
            file.chmod(0o755)
        self.args = argparse.Namespace(state_dir=self.root/'state with spaces', desktop_app=self.desktop,
            aitocraft=self.files[0], gridcraft=self.files[1], node='/fixture/node',
            no_register_links=True, install_skill=False, replace_skill=False,
            skill_dir=self.root/'test skills/gridcraft-chat-experimental')

    def test_configuration_and_import_work_from_another_directory_without_starting_apps(self):
        with patch('setup.subprocess.run') as run:
            setup.setup(self.args)
        self.assertEqual(len(run.call_args_list), 1)  # Only the Node capability check.
        config = json.loads((self.args.state_dir/'installation.json').read_text())
        self.assertEqual(config['gridcraft'], str(self.files[1]))
        env = dict(os.environ, GRIDCRAFT_CODEX_STATE_DIR=str(self.args.state_dir), PYTHONPATH=str(setup.HERE),
                   PYTHONDONTWRITEBYTECODE='1')
        actual = subprocess.check_output([sys.executable, '-c',
            'import runtime_paths as r,json; print(json.dumps([str(r.LAB),str(r.GRIDCRAFT_BIN)]))'], cwd=self.root, env=env, text=True)
        self.assertEqual(json.loads(actual), [str(self.args.state_dir), str(self.files[1])])
        launch = (self.args.state_dir/'Start Codex Desktop.command').read_text()
        self.assertIn('GRIDCRAFT_CODEX_STATE_DIR=', launch)
        self.assertNotIn('--thread', launch)
        self.assertFalse((self.args.state_dir/'codex-desktop-runtime.json').exists())

    def test_skill_install_is_explicit_and_replacement_keeps_backup_outside_discovery(self):
        self.args.install_skill = True
        with patch('setup.subprocess.run'):
            setup.setup(self.args)
            target = self.args.skill_dir
            config = json.loads((target/'runtime.json').read_text())
            self.assertEqual(config['stateDir'], str(self.args.state_dir))
            self.assertEqual(config['helper'], str(setup.HERE/'gridcraft_links.py'))
            with self.assertRaises(ValueError):
                setup.setup(self.args)
            self.args.replace_skill = True
            setup.setup(self.args)
            self.assertEqual(len(list((self.args.state_dir/'skill-backups').iterdir())), 1)
            self.assertEqual(len(list(target.parent.iterdir())), 1)


if __name__ == '__main__':
    unittest.main()

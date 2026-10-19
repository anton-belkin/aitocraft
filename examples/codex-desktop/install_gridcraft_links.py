#!/usr/bin/env python3
"""Build/register the local macOS protocol handler; never modifies Desktop or Claude settings."""
from pathlib import Path
import plistlib
import subprocess

from runtime_paths import HERE, LAB
APP = LAB/'apps/GridCraft Link.app'


def main():
    macos = APP/'Contents/MacOS'
    macos.mkdir(parents=True, exist_ok=True)
    subprocess.run(['/usr/bin/xcrun', 'swiftc', str(HERE/'GridCraftLink.swift'), '-o', str(macos/'GridCraftLink'),
                    '-module-cache-path', str(LAB/'build/swift-module-cache')], check=True)
    info = {'CFBundleName': 'GridCraft Link', 'CFBundleIdentifier': 'org.aitocraft.gridcraft-link',
            'CFBundleExecutable': 'GridCraftLink', 'CFBundlePackageType': 'APPL', 'CFBundleVersion': '1',
            'LSUIElement': True, 'GridCraftLinkHelper': str(HERE/'gridcraft_links.py'),
            'GridCraftStateDirectory': str(LAB),
            'CFBundleURLTypes': [{'CFBundleURLName': 'GridCraft chat link', 'CFBundleURLSchemes': ['gridcraft'],
                                  'CFBundleTypeRole': 'Viewer'}]}
    with (APP/'Contents/Info.plist').open('wb') as out:
        plistlib.dump(info, out)
    subprocess.run(['/usr/bin/codesign', '--force', '--sign', '-', str(APP)], check=True)
    subprocess.run(['/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister',
                    '-f', str(APP)], check=True)
    print(APP)


if __name__ == '__main__':
    main()

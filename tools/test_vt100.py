"""Run bundled vt100 unit/doc tests without its unused upstream dev dependencies.

The application suite does not run dependency unit tests. This isolated harness
uses the actual vendored library source and writes only inside target/.
Usage: python tools/test_vt100.py
"""
import os
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[1]
HARNESS = ROOT / 'target' / 'vt100-unit-tests'
MANIFEST = '''[package]
name = "vt100-unit-tests"
version = "0.1.0"
edition = "2024"
[lib]
name = "vt100"
path = "../../vendor/vt100/src/lib.rs"
[features]
default = ["std"]
std = ["vte/std"]
embedded-io = []
[dependencies]
itoa = "1.0.18"
unicode-width = "0.2.3"
vte = { version = "0.15.0", default-features = false }
'''


def main():
    HARNESS.mkdir(parents=True, exist_ok=True)
    manifest = HARNESS / 'Cargo.toml'
    manifest.write_text(MANIFEST, encoding='utf-8')
    environment = dict(os.environ)
    environment['CARGO_TARGET_DIR'] = str(HARNESS / 'build')
    return subprocess.call(['cargo', 'test', '--offline', '--manifest-path', str(manifest)],
                           cwd=str(ROOT), env=environment)


if __name__ == '__main__':
    raise SystemExit(main())

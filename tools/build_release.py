"""Build and package a release with remapped paths and verifiable contents.

Usage: python tools/build_release.py [--offline]
Supports native Windows x86_64 MSVC and Linux x86_64 GNU builds.
"""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import tarfile
import zipfile


ROOT = Path(__file__).resolve().parents[1]
DOCUMENTS = ['LICENSE', 'THIRD_PARTY_NOTICES.md', 'README.md', 'SECURITY.md', 'COMPATIBILITY.md', 'VALIDATION.md', 'UI_COMPARISON.md']


def output(command):
    return subprocess.check_output(command, cwd=str(ROOT), universal_newlines=True).strip()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def source_hashes():
    files = [ROOT / 'Cargo.toml', ROOT / 'Cargo.lock'] + sorted((ROOT / 'src').rglob('*.rs')) + sorted(p for p in (ROOT / 'vendor').rglob('*') if p.is_file())
    return dict((p.relative_to(ROOT).as_posix(), digest(p.read_bytes())) for p in files)


def pe_imports(data):
    pe = struct.unpack_from('<I', data, 0x3c)[0]
    if data[pe:pe + 4] != b'PE\0\0' or struct.unpack_from('<H', data, pe + 4)[0] != 0x8664:
        raise ValueError('expected a Windows x86_64 PE executable')
    count, size = struct.unpack_from('<H', data, pe + 6)[0], struct.unpack_from('<H', data, pe + 20)[0]
    optional = pe + 24
    if struct.unpack_from('<H', data, optional)[0] != 0x20b:
        raise ValueError('expected PE32+')
    sections = []
    for n in range(count):
        at = optional + size + 40 * n
        virtual_size, virtual, raw_size, raw = struct.unpack_from('<IIII', data, at + 8)
        sections.append((virtual, max(virtual_size, raw_size), raw))

    def offset(rva):
        for virtual, length, raw in sections:
            if virtual <= rva < virtual + length:
                return raw + rva - virtual
        raise ValueError('invalid PE RVA')

    rva = struct.unpack_from('<I', data, optional + 112 + 8)[0]
    imports = []
    if rva:
        at = offset(rva)
        while any(data[at:at + 20]):
            name = offset(struct.unpack_from('<I', data, at + 12)[0])
            imports.append(data[name:data.index(b'\0', name)].decode('ascii'))
            at += 20
    if any(re.match(r'(vcruntime|msvcp|ucrtbase|api-ms-win-crt)', name, re.I) for name in imports):
        raise ValueError('release requires an external Visual C++ runtime: ' + ', '.join(imports))
    return sorted(imports)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--offline', action='store_true')
    parser.add_argument('--output', default=str(ROOT / 'dist' / 'release'))
    args = parser.parse_args()
    rust = output(['rustc', '-vV'])
    target = re.search(r'^host: (.+)$', rust, re.M).group(1)
    if target not in ('x86_64-pc-windows-msvc', 'x86_64-unknown-linux-gnu'):
        parser.error('unvalidated release target: ' + target)
    windows = target.endswith('windows-msvc')
    env = os.environ.copy()
    flags = ['--remap-path-prefix=' + str(Path.home()) + '=/user',
             '--remap-path-prefix=' + str(ROOT) + '=/src/pwsh-tmux']
    if env.get('CARGO_HOME'):
        flags.append('--remap-path-prefix=' + env['CARGO_HOME'] + '=/cargo')
    if windows:
        flags += ['-C', 'target-feature=+crt-static']
    env['CARGO_ENCODED_RUSTFLAGS'] = '\x1f'.join(flags)
    command = ['cargo', 'build', '--release', '--locked', '--target', target]
    if args.offline:
        command.append('--offline')
    source = source_hashes()
    subprocess.check_call(command, cwd=str(ROOT), env=env)
    if source_hashes() != source:
        raise ValueError('source changed while building; rebuild before packaging')
    build = Path(env.get('CARGO_TARGET_DIR', str(ROOT / 'target')))
    if not build.is_absolute():
        build = ROOT / build
    executable = build / target / 'release' / ('pwsh-tmux.exe' if windows else 'pwsh-tmux')
    data = executable.read_bytes()
    paths = [str(Path.home()), str(ROOT)]
    for path in paths:
        for spelling in (path, path.replace('\\', '/'), path.replace('/', '\\')):
            for encoding in ('utf-8', 'utf-16le'):
                if spelling.encode(encoding).lower() in data.lower():
                    raise ValueError('unmapped build path in executable')
    imports = pe_imports(data) if windows else []
    if not windows and (data[:5] != b'\x7fELF\x02' or struct.unpack_from('<H', data, 18)[0] != 62):
        raise ValueError('expected a Linux x86_64 ELF executable')
    version = re.search(r'^version = "([^"]+)"', (ROOT / 'Cargo.toml').read_text(), re.M).group(1)
    name = 'tmux-' + version + '-' + target
    files = dict((document, (ROOT / document).read_bytes()) for document in DOCUMENTS)
    files['tmux.exe' if windows else 'tmux'] = data
    manifest = dict(version=version, target=target, rustc=rust.splitlines()[0], cargo=output(['cargo', '-V']),
                    git_commit=output(['git', 'rev-parse', 'HEAD']),
                    git_dirty=bool(output(['git', 'status', '--porcelain'])),
                    source_sha256=source,
                    files_sha256=dict((path, digest(content)) for path, content in sorted(files.items())),
                    windows_imports=imports, static_msvc_runtime=windows)
    files['BUILDINFO.json'] = (json.dumps(manifest, indent=2, sort_keys=True) + '\n').encode()
    files['SHA256SUMS'] = ''.join(digest(content) + '  ' + path + '\n' for path, content in sorted(files.items())).encode()
    destination = Path(args.output).resolve()
    destination.mkdir(parents=True, exist_ok=True)
    archive = destination / (name + ('.zip' if windows else '.tar.gz'))
    if windows:
        with zipfile.ZipFile(str(archive), 'w', zipfile.ZIP_DEFLATED) as bundle:
            for path, content in sorted(files.items()):
                entry = zipfile.ZipInfo(name + '/' + path, (1980, 1, 1, 0, 0, 0))
                entry.compress_type = zipfile.ZIP_DEFLATED
                entry.external_attr = (0o755 if path == 'tmux.exe' else 0o644) << 16
                bundle.writestr(entry, content)
    else:
        with archive.open('wb') as raw, gzip.GzipFile(fileobj=raw, mode='wb', filename='', mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w') as bundle:
                for path, content in sorted(files.items()):
                    entry = tarfile.TarInfo(name + '/' + path)
                    entry.size = len(content)
                    entry.mode = 0o755 if path == 'tmux' else 0o644
                    bundle.addfile(entry, io.BytesIO(content))
    checksum = digest(archive.read_bytes())
    # bytes, so Windows builds get LF line endings too (sha256sum -c rejects CRLF)
    Path(str(archive) + '.sha256').write_bytes((checksum + '  ' + archive.name + '\n').encode())
    print(json.dumps(dict(archive=str(archive), sha256=checksum, target=target, windows_imports=imports), indent=2))


if __name__ == '__main__':
    main()

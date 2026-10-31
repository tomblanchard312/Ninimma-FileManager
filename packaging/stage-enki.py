#!/usr/bin/env python3
"""Stage a pinned binary-only Enki release; never fetch or build private source."""
import argparse
import hashlib
import json
import os
import platform
import shutil
from pathlib import Path
import stat
import tempfile
import urllib.request
import zipfile

LIMIT = 1024 * 1024 * 1024
BINARIES = {'sharedai-daemon', 'sharedai-worker', 'sharedai-sd', 'whisper-cli'}
NOTICES = {'BINARY-DISTRIBUTION-LICENSE.txt', 'THIRD-PARTY-NOTICES.txt', 'NATIVE-LICENSES.txt', 'WHISPER-LICENSE.txt'}


def sha256_stream(stream):
    digest = hashlib.sha256()
    while chunk := stream.read(1024 * 1024):
        digest.update(chunk)
    return digest.hexdigest()


def stage(archive, sha256, target, version, output):
    if archive.is_symlink() or not archive.is_file() or archive.stat().st_size > LIMIT:
        raise ValueError('Invalid or oversized runtime archive')
    with archive.open('rb') as stream:
        if sha256_stream(stream) != sha256:
            raise ValueError('Runtime archive checksum mismatch')
    suffix = '.exe' if 'windows' in target else ''
    executables = {name + suffix for name in BINARIES}
    library_name = 'sd_enki.dll' if suffix else 'libsd_enki.dylib' if 'apple' in target else 'libsd_enki.so'
    allowed = executables | NOTICES | {library_name, 'runtime.json'}
    with zipfile.ZipFile(archive) as package:
        members = package.infolist()
        names = {m.filename for m in members}
        if len(names) != len(members) or not names <= allowed or not executables | NOTICES | {library_name, 'runtime.json'} <= names:
            raise ValueError('Unexpected runtime contents; only binaries and notices are permitted')
        if sum(m.file_size for m in members) > 2 * LIMIT:
            raise ValueError('Runtime payload is oversized')
        for member in members:
            limit = 16384 if member.filename == 'runtime.json' else LIMIT
            if member.is_dir() or stat.S_IFMT(member.external_attr >> 16) not in (0, stat.S_IFREG) or not 0 < member.file_size <= limit or member.flag_bits & 1:
                raise ValueError('Invalid runtime member')
        metadata = json.loads(package.read('runtime.json'))
        if not isinstance(metadata, dict) or type(metadata.get('schema')) is not int or metadata.get('schema') != 1 or not isinstance(metadata.get('files'), dict) or metadata.get('target') != target or metadata.get('version') != version or set(metadata.get('files', {})) != names - {'runtime.json'}:
            raise ValueError('Runtime identity mismatch')
        # Verify the entire payload before touching the package destination.
        for name, digest in metadata['files'].items():
            with package.open(name) as stream:
                if sha256_stream(stream) != digest:
                    raise ValueError('Runtime member checksum mismatch')
        if output.exists() or output.is_symlink():
            raise ValueError('Runtime destination already exists')
        output.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=output.parent) as temporary:
            payload = Path(temporary) / 'enki'
            payload.mkdir()
            for member in members:
                path = payload / member.filename
                with package.open(member) as source, path.open('xb') as dest:
                    shutil.copyfileobj(source, dest, 1024 * 1024)
                path.chmod(0o755 if member.filename in executables else 0o644)
            os.replace(payload, output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    config = json.loads(Path(__file__).with_name('enki-runtime.json').read_text())
    artifact = config['targets'].get(args.target)
    if not artifact:
        raise ValueError(f'No reviewed Enki binary archive for {args.target}; packaging cannot omit the runtime')
    if 'linux' in args.target and platform.system() == 'Linux' and artifact.get('build_glibc'):
        library, version = platform.libc_ver()
        if library != 'glibc' or tuple(map(int, version.split('.'))) < tuple(map(int, artifact['build_glibc'].split('.'))):
            raise ValueError('The reviewed Enki preview requires glibc ' + artifact['build_glibc'] + '; obtain a compatible runtime build before packaging')
    with tempfile.TemporaryDirectory() as temporary:
        archive = Path(temporary) / 'runtime.zip'
        local = os.environ.get('ENKI_RUNTIME_ARCHIVE')
        if local:
            archive = Path(local)
        else:
            url = f'https://github.com/tomblanchard312/Enki-runtime/releases/download/v{config["version"]}/{artifact["name"]}'
            with urllib.request.urlopen(url, timeout=120) as response, archive.open('xb') as dest:
                size = 0
                while chunk := response.read(1024 * 1024):
                    size += len(chunk)
                    if size > LIMIT:
                        raise ValueError('Runtime download is oversized')
                    dest.write(chunk)
        stage(archive, artifact['sha256'], args.target, config['version'], args.out)


if __name__ == '__main__':
    main()

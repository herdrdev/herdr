import argparse
import hashlib
import json
import os
import re
import subprocess
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TARGETS = (
    'aarch64-apple-darwin', 'x86_64-apple-darwin',
    'x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl',
    'x86_64-pc-windows-msvc',
)


def output(args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def decode_zon_string(text, key):
    matches = re.findall(r'^\s*\.' + re.escape(key) + r'\s*=\s*("(?:[^"\\]|\\.)*"),\s*$', text, re.MULTILINE)
    if len(matches) != 1:
        raise ValueError('Zig environment does not identify its installation')
    try:
        return json.loads(matches[0])
    except json.JSONDecodeError as error:
        raise ValueError('Unsupported Zig environment path') from error


def zon_string(text, key):
    value = decode_zon_string(text, key)
    if not value or not Path(value).is_absolute():
        raise ValueError('Zig installation path must be absolute')
    return Path(value)


def collect(rustc='rustc', zig='zig'):
    rust = output([rustc, '-vV'])
    lines = rust.splitlines()
    if not lines or not re.fullmatch(r'rustc 1\.96\.1(?: \([0-9a-f]+ [0-9-]+\))?', lines[0]) or 'release: 1.96.1' not in lines:
        raise ValueError('Rust 1.96.1 is required')
    if any(not re.fullmatch(r'[A-Za-z0-9 .:+_()\-]+', line) for line in lines):
        raise ValueError('Rust version output is not safe to record')
    root = Path(output([rustc, '--print', 'sysroot']))
    if not root.is_absolute():
        raise ValueError('Rust sysroot must be absolute')
    version = output([zig, 'version'])
    if version != '0.16.0':
        raise ValueError('Zig 0.16.0 is required')
    env = output([zig, 'env'])
    exe = zon_string(env, 'zig_exe')
    lib = zon_string(env, 'lib_dir')
    if os.environ.get('ZIG_LIB_DIR'):
        lib = Path(os.environ['ZIG_LIB_DIR']).resolve()
    sources = {
        'rust/COPYRIGHT-library.html': root / 'share/doc/rust/COPYRIGHT-library.html',
        'zig/LICENSE': exe.parent / 'LICENSE',
        'zig/musl/COPYRIGHT': lib / 'libc/musl/COPYRIGHT',
    }
    files = {}
    for name, path in sources.items():
        if not path.is_file():
            raise ValueError(f'Required local toolchain notice missing: {name}')
        data = path.read_bytes()
        if not data:
            raise ValueError(f'Required local toolchain notice empty: {name}')
        files[name] = data
    return files, rust, version


def write_archive(output_path, target, commit, files, rust, zig):
    if target not in TARGETS or not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('Invalid target or source commit')
    if set(files) != {'rust/COPYRIGHT-library.html', 'zig/LICENSE', 'zig/musl/COPYRIGHT'}:
        raise ValueError('Unexpected toolchain notices')
    metadata = {'alvo': target, 'fonte_commit': commit, 'rustc_vV': rust, 'zig_version': zig,
                'arquivos': {name: hashlib.sha256(data).hexdigest() for name, data in sorted(files.items())}}
    payload = dict(files)
    payload['origem.json'] = (json.dumps(metadata, indent=2, ensure_ascii=True) + '\n').encode()
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.toolchain-', dir=output_path.parent) as temporary:
        staged = Path(temporary) / 'toolchain.zip'
        with zipfile.ZipFile(staged, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
            for name, data in sorted(payload.items()):
                info = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                info.external_attr = 0o100644 << 16
                archive.writestr(info, data)
        os.link(staged, output_path)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--target', required=True, choices=TARGETS)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--rustc', default=os.environ.get('RUSTC') or 'rustc')
    parser.add_argument('--zig', default=os.environ.get('ZIG') or 'zig')
    args = parser.parse_args()
    files, rust, zig = collect(args.rustc, args.zig)
    commit = output(['git', 'rev-parse', 'HEAD'])
    write_archive(args.output, args.target, commit, files, rust, zig)


if __name__ == '__main__':
    main()

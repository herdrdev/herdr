import argparse
import hashlib
import json
import io
from pathlib import Path
import re
import stat
import struct
import subprocess
import tempfile
import zipfile

try:
    from scripts import package_windows_conpty as conpty
except ModuleNotFoundError:
    import package_windows_conpty as conpty


ROOT = Path(__file__).resolve().parents[1]
TARGETS = {"macos-arm64": "aarch64-apple-darwin", "linux-x64": "x86_64-unknown-linux-musl",
           "windows-x64": "x86_64-pc-windows-msvc"}
LIMIT = 256 * 1024 * 1024


def read_file(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > LIMIT:
        raise ValueError(f"Invalid input file: {path.name}")
    return path.read_bytes()


def validate_rust_notices(path, target, commit):
    payload = read_file(path)
    with zipfile.ZipFile(io.BytesIO(payload)) as archive:
        members = archive.infolist()
        names = [item.filename for item in members]
        if len(names) != len(set(names)) or len(names) > 1024 or sum(m.file_size for m in members) > 16 * 1024 * 1024:
            raise ValueError("Invalid Rust notices archive")
        manifest = json.loads(archive.read("origem.json"))
        if manifest.get("alvo") != target or not manifest.get("pacotes"):
            raise ValueError("Rust notices target mismatch or empty inventory")
        if manifest.get("fonte_commit") != commit:
            raise ValueError("Rust notices source commit mismatch")
        expected = {"origem.json"}
        for package in manifest["pacotes"]:
            if not package.get("pacote") or not package.get("versao") or not package.get("textos"):
                raise ValueError("Package without license texts")
            for item in package["textos"]:
                digest = item["sha256"]
                if not item.get("fonte"):
                    raise ValueError("License text has no source")
                if not re.fullmatch(r"[a-f0-9]{64}", digest) or item["arquivo"] != f"textos/{digest}.txt":
                    raise ValueError("Invalid license text path")
                if hashlib.sha256(archive.read(item["arquivo"])).hexdigest() != digest:
                    raise ValueError("License text hash mismatch")
                expected.add(item["arquivo"])
        if set(names) != expected or any(stat.S_IFMT(m.external_attr >> 16) not in (0, stat.S_IFREG) for m in members):
            raise ValueError("Unexpected Rust notices content")
    return payload


def collect(platform, source, rust_notices, commit, root=ROOT):
    target = TARGETS[platform]
    files = {"licencas/RUST-LICENSES.zip": validate_rust_notices(rust_notices, target, commit)}
    for name, origin in (("HERDR-APACHE-2.0.txt", "LICENSE"),
                         ("GHOSTTY.txt", "vendor/libghostty-vt/LICENSE"),
                         ("PORTABLE-PTY.md", "vendor/portable-pty/LICENSE.md")):
        files[f"licencas/{name}"] = read_file(root / origin)
    for name in ("wuffs/LICENSE", "wuffs/LICENSE-MIT", "wuffs/LICENSE-APACHE", "uucode/LICENSE.md",
                 "uucode/licenses/LICENSE_Bjoern_Hoehrmann", "uucode/licenses/LICENSE_unicode"):
        files[f"licencas/{name}"] = read_file(root / "packaging/licenses" / name)
    if platform == "windows-x64":
        if source.is_symlink() or any(p.is_symlink() for p in source.rglob("*")):
            raise ValueError("Windows stage contains links")
        conpty.validate_stage(root / "packaging/windows/conpty.json", "x86_64", source)
        for path in sorted(source.rglob("*")):
            if path.is_file():
                name = path.relative_to(source).as_posix()
                files["herdr-pretorian.exe" if name == "herdr.exe" else name] = read_file(path)
        if conpty.pe_machine(files["herdr-pretorian.exe"]) != 0x8664:
            raise ValueError("Wrong Windows executable architecture")
    else:
        binary = read_file(source)
        if platform == "linux-x64":
            valid = len(binary) >= 20 and binary[:6] == b"\x7fELF\x02\x01" and struct.unpack_from("<H", binary, 18)[0] == 62
        else:
            valid = len(binary) >= 8 and struct.unpack_from("<II", binary) == (0xFEEDFACF, 0x0100000C)
        if not valid:
            raise ValueError("Wrong executable architecture")
        files["herdr-pretorian"] = binary
    if len(files) > 256 or sum(map(len, files.values())) > LIMIT:
        raise ValueError("Candidate exceeds installer limits")
    return files


def verify_build(files, platform, source_name, build_info, commit):
    fields, hashes = {}, {}
    for line in build_info.splitlines():
        if line.startswith("sha256="):
            match = re.fullmatch(r"sha256=([a-f0-9]{64})  (.+)", line)
            if not match or match[2] in hashes:
                raise ValueError("Invalid or duplicate build hash")
            hashes[match[2]] = match[1]
        elif "=" in line:
            key, value = line.split("=", 1)
            if key in fields:
                raise ValueError("Duplicate build field")
            fields[key] = value
    if fields.get("commit") != commit or fields.get("target") != TARGETS[platform]:
        raise ValueError("Build source or target mismatch")
    sources = {"RUST-LICENSES.zip": files["licencas/RUST-LICENSES.zip"]}
    if platform == "windows-x64":
        sources.update({("herdr.exe" if name == "herdr-pretorian.exe" else name): data
                        for name, data in files.items() if not name.startswith("licencas/")})
    else:
        sources[source_name] = files["herdr-pretorian"]
    for name, data in sources.items():
        if hashes.get(name) != hashlib.sha256(data).hexdigest():
            raise ValueError(f"File is not bound to this build: {name}")


def package(platform, files, commit, output):
    if not re.fullmatch(r"[a-f0-9]{40}", commit):
        raise ValueError("Full source commit required")
    if output.exists() or output.is_symlink():
        raise ValueError("Candidate output already exists")
    output.parent.mkdir(parents=True, exist_ok=True)
    version = "candidato-" + commit[:12]
    name = f"pretorian-fork-{platform}-{commit[:12]}.zip"
    with tempfile.TemporaryDirectory(prefix=".candidate-", dir=output.parent) as temporary:
        staging = Path(temporary) / "candidate"
        staging.mkdir()
        archive_path = staging / name
        with zipfile.ZipFile(archive_path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for relative, payload in sorted(files.items()):
                info = zipfile.ZipInfo(relative, (1980, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                info.external_attr = (0o100755 if relative.startswith("herdr-pretorian") else 0o100644) << 16
                archive.writestr(info, payload)
        if archive_path.stat().st_size > LIMIT:
            raise ValueError("Candidate download exceeds installer limit")
        metadata = {"estado": "candidato", "repositorio": "lamarck-nemesis/herdr-pretorian",
                    "versao": version, "fonte_commit": commit,
                    "artefatos": {platform: {"arquivo": name,
                        "sha256": hashlib.sha256(archive_path.read_bytes()).hexdigest(),
                        "arquivos": {n: hashlib.sha256(v).hexdigest() for n, v in sorted(files.items())}}}}
        (staging / "candidato.json").write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
        staging.rename(output)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--platform", choices=TARGETS, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--rust-notices", type=Path, required=True)
    parser.add_argument("--build-info", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    subprocess.run(["git", "diff", "--quiet", "HEAD"], cwd=ROOT, check=True)
    files = collect(args.platform, args.source, args.rust_notices, commit)
    verify_build(files, args.platform, args.source.name,
                 args.build_info.read_text(encoding="utf-8-sig"), commit)
    package(args.platform, files, commit, args.output)


if __name__ == "__main__":
    main()

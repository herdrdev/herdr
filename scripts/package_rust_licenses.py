import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import zipfile


ROOT = Path(__file__).resolve().parents[1]
TARGETS = (
    "aarch64-apple-darwin", "x86_64-apple-darwin",
    "x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl",
    "x86_64-pc-windows-msvc",
)


def collect(report, root):
    packages = {(p["package"]["name"], p["package"]["version"]): p["package"]
                for p in report["crates"]}
    directories = [(Path(p["manifest_path"]).parent.resolve(), key)
                   for key, p in packages.items()]
    directories.sort(key=lambda item: len(item[0].parts), reverse=True)
    base = root / "packaging/licenses/rust"
    supplements = json.loads((base / "fontes-complementares.json").read_text(encoding="utf-8"))
    fallback = {}
    for record in supplements["arquivos"]:
        path = (base / record["arquivo"]).resolve()
        if base.resolve() not in path.parents:
            raise ValueError("License supplement escapes its directory")
        data = path.read_bytes()
        if hashlib.sha256(data).hexdigest() != record["sha256"]:
            raise ValueError("License supplement hash mismatch")
        for source in record["fontes"]:
            key = (source["pacote"], source["versao"])
            fallback.setdefault(key, []).append((data, source["fonte"]))
    fallback[("ghostty-vt", "0.0.0")] = [(root.joinpath("LICENSE").read_bytes(), "Herdr/LICENSE")]
    files = {}
    entries = {key: [] for key in packages}
    for license_group in report["licenses"]:
        original = None
        if license_group.get("source_path"):
            source = (root / license_group["source_path"]).resolve()
            owner = next(((directory, key) for directory, key in directories
                          if directory in source.parents
                          and (directory != root.resolve() or source == root.resolve() / "LICENSE")), None)
            if owner is None:
                raise ValueError("License source is outside the dependency sources")
            directory, key = owner
            data = source.read_bytes()
            if data.decode("utf-8") != license_group.get("text"):
                raise ValueError("License source changed since the report was generated")
            original = [(data, "/".join((*key, source.relative_to(directory).as_posix())))]
        for used in license_group["used_by"]:
            key = (used["crate"]["name"], used["crate"]["version"])
            if key not in entries:
                raise ValueError("License refers to a package outside the report")
            sources = original or fallback.get(key)
            if not sources:
                raise ValueError(f"Original license text missing: {key[0]} {key[1]}")
            for data, provenance in sources:
                digest = hashlib.sha256(data).hexdigest()
                name = f"textos/{digest}.txt"
                files[name] = data
                entry = {"arquivo": name, "sha256": digest, "fonte": provenance}
                if entry not in entries[key]:
                    entries[key].append(entry)
    if any(not texts for texts in entries.values()):
        raise ValueError("A package has no original license text")
    inventory = [{"pacote": name, "versao": version,
                  "licenca_declarada": packages[(name, version)]["license"],
                  "textos": sorted(texts, key=lambda item: (item["arquivo"], item["fonte"]))}
                 for (name, version), texts in sorted(entries.items())]
    return files, inventory


def write_archive(output, target, files, inventory, source_commit):
    if target not in TARGETS:
        raise ValueError("Unsupported target")
    manifest = {"alvo": target, "gerador": "cargo-about 0.9.3", "fonte_commit": source_commit,
                "escopo": "dependencias declaradas, incluindo build e excluindo desenvolvimento",
                "pacotes": inventory}
    payload = dict(files)
    payload["origem.json"] = (json.dumps(manifest, ensure_ascii=False, indent=2) + "\n").encode()
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".licenses-", dir=output.parent) as temporary:
        staged = Path(temporary) / "licenses.zip"
        with zipfile.ZipFile(staged, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for name, data in sorted(payload.items()):
                info = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                info.external_attr = 0o100644 << 16
                archive.writestr(info, data)
        os.link(staged, output)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cargo-about", default="cargo-about")
    args = parser.parse_args()
    version = subprocess.check_output([args.cargo_about, "--version"], text=True).strip()
    if version != "cargo-about 0.9.3":
        raise ValueError("cargo-about 0.9.3 is required")
    with tempfile.TemporaryDirectory(prefix="herdr-licenses-") as temporary:
        report = Path(temporary) / "report.json"
        subprocess.run([args.cargo_about, "generate", "--locked", "--fail", "--target", args.target,
                        "--format", "json", "--config", str(ROOT / "packaging/licenses/about.toml"),
                        "--output-file", str(report)], cwd=ROOT, check=True)
        files, inventory = collect(json.loads(report.read_text(encoding="utf-8")), ROOT)
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    write_archive(args.output, args.target, files, inventory, commit)


if __name__ == "__main__":
    main()

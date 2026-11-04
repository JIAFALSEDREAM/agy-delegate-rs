#!/usr/bin/env python3
"""Validate tagged releases and assemble the same binaries for GitHub and npm."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
DOCUMENTS = ("README.md", "README.en.md", "INSTALL.md", "INSTALL.en.md")
TARGETS = {
    "x86_64-unknown-linux-musl": "linux-x64",
    "aarch64-unknown-linux-musl": "linux-arm64",
    "x86_64-apple-darwin": "darwin-x64",
    "aarch64-apple-darwin": "darwin-arm64",
    "x86_64-pc-windows-msvc": "win32-x64",
    "aarch64-pc-windows-msvc": "win32-arm64",
}
VERSION_FILES = ("Cargo.toml", "Cargo.lock", "npm/package.json")


def command(*args, cwd=None):
    return subprocess.check_output(args, cwd=ROOT if cwd is None else cwd, text=True).strip()


def repository_url(repository):
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("repository must be a GitHub owner/name")
    return f"https://github.com/{repository}"


def platform_dependencies(version):
    return {f"agy-delegate-rs-{platform}": version for platform in TARGETS.values()}


def validate_dependencies(manifest):
    if manifest.get("optionalDependencies") != platform_dependencies(manifest["version"]):
        raise ValueError("npm platform dependencies must cover the complete matrix at the entry package version")


def release_version(tag):
    if not re.fullmatch(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", tag):
        raise ValueError("release tags must be stable versions: vMAJOR.MINOR.PATCH")
    return tag[1:]


def check_version_changes():
    changed = command("git", "diff", "--name-only", "HEAD").splitlines()
    untracked = command("git", "ls-files", "--others", "--exclude-standard").splitlines()
    unexpected = set(changed + untracked) - set(VERSION_FILES)
    if unexpected:
        raise ValueError(f"release checkout contains unrelated changes: {sorted(unexpected)}")


def with_package_version(text, name, version, lock=False):
    heading = r"\[\[package\]\]" if lock else r"\[package\]"
    sections = re.finditer(rf"(?ms)^{heading}[^\n]*\n.*?(?=^\[|\Z)", text)
    matches = []
    for section in sections:
        package = tomllib.loads(section.group())["package"]
        if lock:
            package = package[0]
        if package.get("name") == name and (not lock or "source" not in package):
            matches.append(section)
    if len(matches) != 1:
        raise ValueError(f"expected exactly one root package section for {name}")
    section = matches[0]
    updated, count = re.subn(
        r'''(?m)^([ \t]*version[ \t]*=[ \t]*)(?:"[^"\r\n]*"|'[^'\r\n]*')''',
        lambda match: f'{match[1]}"{version}"', section.group(),
    )
    if count != 1:
        raise ValueError(f"expected exactly one package version for {name}")
    return text[:section.start()] + updated + text[section.end():]


def prepare(tag):
    version = release_version(tag)
    check_version_changes()
    cargo_text = (ROOT / "Cargo.toml").read_text()
    name = tomllib.loads(cargo_text)["package"]["name"]
    npm = json.loads((ROOT / "npm/package.json").read_text())
    dependencies = platform_dependencies(version)
    if set(npm.get("optionalDependencies", {})) != set(dependencies):
        raise ValueError("npm platform dependencies must cover the complete matrix")
    npm["version"] = version
    npm["optionalDependencies"] = dependencies
    # Prepare all content before writing, retaining dependency versions and TOML comments.
    contents = (
        with_package_version(cargo_text, name, version),
        with_package_version((ROOT / "Cargo.lock").read_text(), name, version, lock=True),
        json.dumps(npm, indent=2) + "\n",
    )
    for path, content in zip(VERSION_FILES, contents):
        (ROOT / path).write_text(content, encoding="utf-8")
    print(f"Prepared release version {version} from {tag}; no commit created")


def validate(tag, main_ref, repository):
    version = release_version(tag)
    cargo = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]
    npm = json.loads((ROOT / "npm/package.json").read_text())
    packages = tomllib.loads((ROOT / "Cargo.lock").read_text())["package"]
    roots = [p for p in packages if p["name"] == cargo["name"] and "source" not in p]
    if (cargo["version"] != version or npm["version"] != version
            or len(roots) != 1 or roots[0]["version"] != version):
        raise ValueError("tag, Cargo.toml, Cargo.lock and npm/package.json versions must match")
    validate_dependencies(npm)
    commit = command("git", "rev-parse", f"refs/tags/{tag}^{{commit}}")
    if commit != command("git", "rev-parse", "HEAD"):
        raise ValueError("checkout must point to the tagged commit")
    subprocess.run(["git", "merge-base", "--is-ancestor", commit, main_ref], cwd=ROOT, check=True)
    url = repository_url(repository)
    if cargo.get("repository") != url or npm.get("repository", {}).get("url") != f"git+{url}.git":
        raise ValueError("Cargo and npm repository URLs must match the publishing GitHub repository")
    print(f"Validated {tag} on {main_ref}: {commit}")
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a", encoding="utf-8") as stream:
            stream.write(f"version={version}\n")


def archive(target, version, output):
    platform = TARGETS[target]
    binary_name = "agy-delegate-rs.exe" if platform.startswith("win32") else "agy-delegate-rs"
    binary = ROOT / "target" / target / "release" / binary_name
    output.mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary, output / binary_name)
    stem = f"agy-delegate-rs-v{version}-{target}"
    files = [(binary, binary_name)] + [(ROOT / name, name) for name in ("LICENSE", *DOCUMENTS)]
    if platform.startswith("win32"):
        with zipfile.ZipFile(output / f"{stem}.zip", "w", zipfile.ZIP_DEFLATED) as bundle:
            for source, name in files:
                bundle.write(source, f"{stem}/{name}")
    else:
        with tarfile.open(output / f"{stem}.tar.gz", "w:gz") as bundle:
            for source, name in files:
                bundle.add(source, arcname=f"{stem}/{name}", recursive=False)


def assemble(artifacts, output, repository):
    manifest = json.loads((ROOT / "npm/package.json").read_text())
    validate_dependencies(manifest)
    manifest["repository"] = {"type": "git", "url": f"git+{repository_url(repository)}.git"}
    version = manifest["version"]
    # Validate the complete matrix before creating anything intended for publication.
    inputs = []
    for target, platform in TARGETS.items():
        binary_name = "agy-delegate-rs.exe" if platform.startswith("win32") else "agy-delegate-rs"
        binary = artifacts / target / binary_name
        suffix = "zip" if platform.startswith("win32") else "tar.gz"
        bundle = artifacts / target / f"agy-delegate-rs-v{version}-{target}.{suffix}"
        if not binary.is_file() or not bundle.is_file():
            raise ValueError(f"missing build artifacts for {target}")
        inputs.append((platform, binary, bundle))
    output.mkdir(parents=True, exist_ok=False)
    package = output / "npm"
    shutil.copytree(ROOT / "npm", package)
    (package / "package.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    for name in ("LICENSE", *DOCUMENTS):
        shutil.copy2(ROOT / name, package / name)
    assets = output / "assets"
    assets.mkdir()
    for platform, binary, bundle in inputs:
        name = f"agy-delegate-rs-{platform}"
        native = output / "npm-platforms" / name
        destination = native / "bin" / binary.name
        destination.parent.mkdir(parents=True)
        shutil.copy2(binary, destination)
        if not platform.startswith("win32"):
            destination.chmod(0o755)
        system, cpu = platform.split("-")
        metadata = {
            "name": name, "version": version,
            "description": f"Native {platform} binary for agy-delegate-rs",
            "license": manifest.get("license", "MIT"), "repository": manifest["repository"],
            "os": [system], "cpu": [cpu], "files": ["bin", "LICENSE", "README.md"],
        }
        (native / "package.json").write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
        shutil.copy2(ROOT / "LICENSE", native / "LICENSE")
        (native / "README.md").write_text(
            f"# {name}\n\nNative binary installed automatically by `agy-delegate-rs`.\n"
            "Install the CLI with `npm install -g agy-delegate-rs`.\n", encoding="utf-8"
        )
        command("npm", "pack", "--silent", "--ignore-scripts", "--pack-destination", str(assets.resolve()), cwd=native)
        shutil.copy2(bundle, assets / bundle.name)
    command("npm", "pack", "--silent", "--ignore-scripts", "--pack-destination", str(assets.resolve()), cwd=package)
    checksums = []
    for asset in sorted(assets.iterdir()):
        checksums.append(f"{hashlib.sha256(asset.read_bytes()).hexdigest()}  {asset.name}\n")
    (assets / "SHA256SUMS").write_text("".join(checksums), encoding="utf-8")


def reuse_assets(published, output):
    expected = {item.name for item in output.iterdir()}
    if {item.name for item in published.iterdir()} != expected:
        raise ValueError("published Release has a different set of assets")
    lines = (published / "SHA256SUMS").read_text().splitlines()
    checked = set()
    for line in lines:
        digest, name = line.split("  ")
        if name not in expected or name == "SHA256SUMS" or name in checked:
            raise ValueError("invalid published checksum manifest")
        if hashlib.sha256((published / name).read_bytes()).hexdigest() != digest:
            raise ValueError(f"published asset checksum mismatch: {name}")
        checked.add(name)
    if checked != expected - {"SHA256SUMS"}:
        raise ValueError("published checksum manifest is incomplete")
    for npm in output.glob("*.tgz"):
        if npm.read_bytes() != (published / npm.name).read_bytes():
            raise ValueError(f"published npm payload differs from this build: {npm.name}; release assets are immutable")
    for item in published.iterdir():
        shutil.copy2(item, output / item.name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    inject = commands.add_parser("prepare")
    inject.add_argument("--tag", required=True)
    check = commands.add_parser("validate")
    check.add_argument("--tag", required=True)
    check.add_argument("--main-ref", required=True)
    check.add_argument("--repository", required=True)
    pack = commands.add_parser("archive")
    pack.add_argument("--target", choices=TARGETS, required=True)
    pack.add_argument("--version", required=True)
    pack.add_argument("--output", type=Path, required=True)
    collect = commands.add_parser("assemble")
    collect.add_argument("--artifacts", type=Path, required=True)
    collect.add_argument("--output", type=Path, required=True)
    collect.add_argument("--repository", required=True)
    reuse = commands.add_parser("reuse-assets")
    reuse.add_argument("--published", type=Path, required=True)
    reuse.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.action == "prepare":
        prepare(args.tag)
    elif args.action == "validate":
        validate(args.tag, args.main_ref, args.repository)
    elif args.action == "archive":
        archive(args.target, args.version, args.output)
    elif args.action == "assemble":
        assemble(args.artifacts, args.output, args.repository)
    else:
        reuse_assets(args.published, args.output)


if __name__ == "__main__":
    main()

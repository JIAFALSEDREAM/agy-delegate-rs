import contextlib
import hashlib
import io
import json
import runpy
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
import urllib.error
from unittest.mock import patch

import release


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.addCleanup(patch.stopall)
        patch.object(release, "ROOT", self.root).start()
        patch.dict(release.os.environ, {"GITHUB_OUTPUT": ""}).start()
        (self.root / "npm/bin").mkdir(parents=True)
        (self.root / "Cargo.toml").write_text(
            '[package]\nname = "agy-delegate-rs"\nversion = "0.1.0"\nrepository = "https://github.com/example/agy-delegate-rs"\n'
        )
        self.manifest = {
            "name": "agy-delegate-rs", "version": "0.1.0",
            "repository": {"url": "git+https://github.com/example/agy-delegate-rs.git"},
            "bin": {"agy-delegate-rs": "bin/agy-delegate-rs.cjs"},
            "files": ["bin", "README.md", "LICENSE"],
            "optionalDependencies": release.platform_dependencies("0.1.0"),
        }
        (self.root / "npm/package.json").write_text(json.dumps(self.manifest))
        (self.root / "npm/bin/agy-delegate-rs.cjs").write_text("#!/usr/bin/env node\n")
        for name in ("LICENSE", "README.md", "INSTALL.md"):
            (self.root / name).write_text(name)
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Release test")
        self.git("config", "user.email", "release@example.invalid")
        self.git("add", ".")
        self.git("commit", "-m", "fixture")

    def git(self, *args):
        return release.command("git", *args)

    def validate(self, tag="v0.1.0", repository="example/agy-delegate-rs"):
        with contextlib.redirect_stdout(io.StringIO()):
            release.validate(tag, "main", repository)

    def test_annotated_tag_on_main_is_valid(self):
        self.git("tag", "-a", "v0.1.0", "-m", "release")
        self.validate()

    def test_main_history_is_valid_even_after_branch_advances(self):
        self.git("tag", "v0.1.0")
        self.git("commit", "--allow-empty", "-m", "next")
        self.git("checkout", "v0.1.0")
        self.validate()

    def test_side_branch_tag_is_rejected(self):
        self.git("checkout", "-b", "side")
        self.git("commit", "--allow-empty", "-m", "side only")
        self.git("tag", "v0.1.0")
        with self.assertRaises(subprocess.CalledProcessError):
            self.validate()

    def test_bad_versions_and_wrong_repository_are_rejected(self):
        self.git("tag", "v0.1.0")
        for tag in ("v0.1", "v00.1.0", "v0.1.0-rc.1", "v0.1.1"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                self.validate(tag)
        with self.assertRaises(ValueError):
            self.validate(repository="other/agy-delegate-rs")
        self.manifest["version"] = "0.1.1"
        (self.root / "npm/package.json").write_text(json.dumps(self.manifest))
        with self.assertRaises(ValueError):
            self.validate()

    def test_checkout_must_match_tag(self):
        self.git("tag", "v0.1.0")
        self.git("commit", "--allow-empty", "-m", "next")
        with self.assertRaises(ValueError):
            self.validate()

    def test_missing_or_mismatched_platform_dependency_is_rejected(self):
        self.git("tag", "v0.1.0")
        self.manifest["optionalDependencies"]["agy-delegate-rs-linux-arm64"] = "0.0.9"
        (self.root / "npm/package.json").write_text(json.dumps(self.manifest))
        with self.assertRaisesRegex(ValueError, "platform dependencies"):
            self.validate()
        self.manifest["optionalDependencies"].pop("agy-delegate-rs-linux-arm64")
        (self.root / "npm/package.json").write_text(json.dumps(self.manifest))
        with self.assertRaisesRegex(ValueError, "platform dependencies"):
            release.assemble(self.make_artifacts(), self.root / "dist", "example/agy-delegate-rs")

    def make_artifacts(self):
        artifacts = self.root / "artifacts"
        for target, platform in release.TARGETS.items():
            name = "agy-delegate-rs.exe" if platform.startswith("win32") else "agy-delegate-rs"
            binary = self.root / "target" / target / "release" / name
            binary.parent.mkdir(parents=True)
            binary.write_bytes(f"binary:{platform}".encode())
            binary.chmod(0o755)
            release.archive(target, "0.1.0", artifacts / target)
        return artifacts

    def test_missing_matrix_member_prevents_packaging(self):
        artifacts = self.make_artifacts()
        (artifacts / "aarch64-apple-darwin/agy-delegate-rs").unlink()
        output = self.root / "dist"
        with self.assertRaises(ValueError):
            release.assemble(artifacts, output, "example/agy-delegate-rs")
        self.assertFalse(output.exists())

    def test_archives_npm_contents_and_checksums(self):
        artifacts = self.make_artifacts()
        output = self.root / "dist"
        release.assemble(artifacts, output, "example/agy-delegate-rs")
        assets = output / "assets"
        with tarfile.open(assets / "agy-delegate-rs-0.1.0.tgz") as bundle:
            members = {item.name: item for item in bundle.getmembers()}
            self.assertEqual(set(members), {
                "package/package.json", "package/bin/agy-delegate-rs.cjs", "package/README.md", "package/LICENSE",
            })
            manifest = json.load(bundle.extractfile("package/package.json"))
            self.assertEqual(manifest["repository"]["url"], "git+https://github.com/example/agy-delegate-rs.git")
            self.assertEqual(manifest["optionalDependencies"], release.platform_dependencies("0.1.0"))
        for platform in release.TARGETS.values():
            with tarfile.open(assets / f"agy-delegate-rs-{platform}-0.1.0.tgz") as bundle:
                members = {item.name: item for item in bundle.getmembers()}
                name = "agy-delegate-rs.exe" if platform.startswith("win32") else "agy-delegate-rs"
                member = members[f"package/bin/{name}"]
                self.assertEqual(bundle.extractfile(member).read(), f"binary:{platform}".encode())
                if not platform.startswith("win32"):
                    self.assertTrue(member.mode & 0o111)
                self.assertEqual(set(members), {
                    "package/package.json", f"package/bin/{name}", "package/README.md", "package/LICENSE",
                })
                manifest = json.load(bundle.extractfile("package/package.json"))
                self.assertEqual(manifest["name"], f"agy-delegate-rs-{platform}")
                self.assertEqual(manifest["version"], "0.1.0")
                system, cpu = platform.split("-")
                self.assertEqual(manifest["os"], [system])
                self.assertEqual(manifest["cpu"], [cpu])
        for line in (assets / "SHA256SUMS").read_text().splitlines():
            digest, name = line.split("  ")
            self.assertEqual(digest, hashlib.sha256((assets / name).read_bytes()).hexdigest())
        self.assertEqual(len(list(assets.iterdir())), 14)
        # Replace only registry locations for an offline installation test;
        # all native tarballs retain their generated os/cpu restrictions.
        package = output / "npm"
        manifest = json.loads((package / "package.json").read_text())
        manifest["optionalDependencies"] = {
            name: (assets / f"{name}-0.1.0.tgz").as_uri()
            for name in release.platform_dependencies("0.1.0")
        }
        (package / "package.json").write_text(json.dumps(manifest))
        local = output / "local"
        local.mkdir()
        release.command("npm", "pack", "--silent", "--ignore-scripts", "--pack-destination", str(local), cwd=package)
        for platform in release.TARGETS.values():
            with self.subTest(install=platform):
                system, cpu = platform.split("-")
                prefix = output / f"install-{platform}"
                release.command(
                    "npm", "install", "--ignore-scripts", "--offline", "--no-audit", "--no-fund",
                    "--package-lock=false", f"--os={system}", f"--cpu={cpu}", "--prefix", str(prefix),
                    str(local / "agy-delegate-rs-0.1.0.tgz"),
                )
                installed = {p.name for p in (prefix / "node_modules").glob("agy-delegate-rs-*")}
                self.assertEqual(installed, {f"agy-delegate-rs-{platform}"})

    def test_public_release_reuse_verifies_integrity_without_replacing_archives(self):
        output = self.root / "dist"
        release.assemble(self.make_artifacts(), output, "example/agy-delegate-rs")
        assets = output / "assets"
        published = self.root / "published"
        release.shutil.copytree(assets, published)
        # Repacking archive headers must not replace already-published bytes.
        archive = next(item for item in assets.iterdir() if item.name.endswith(".tar.gz"))
        archive.write_bytes(b"repacked")
        release.reuse_assets(published, assets)
        self.assertEqual(archive.read_bytes(), (published / archive.name).read_bytes())
        (published / "agy-delegate-rs-linux-arm64-0.1.0.tgz").write_bytes(b"different valid publication")
        checksums = [
            f"{hashlib.sha256(item.read_bytes()).hexdigest()}  {item.name}\n"
            for item in sorted(published.iterdir()) if item.name != "SHA256SUMS"
        ]
        (published / "SHA256SUMS").write_text("".join(checksums))
        with self.assertRaisesRegex(ValueError, "npm payload differs"):
            release.reuse_assets(published, assets)

    def test_crate_publication_checks_existing_and_ambiguous_uploads(self):
        archive = self.root / "target/package/agy-delegate-rs-0.1.0.crate"
        archive.parent.mkdir(parents=True)
        archive.write_bytes(b"crate fixture")
        checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
        uploader = Path(__file__).with_name("publish-crate.py")
        match = json.dumps({"version": {"checksum": checksum}}).encode()
        missing = urllib.error.HTTPError("test", 404, "missing", {}, None)
        for queries, status, should_publish in [([match], 0, False), ([missing], 0, True), ([missing, match], 1, True)]:
            responses = [io.BytesIO(item) if isinstance(item, bytes) else item for item in queries]
            with patch("subprocess.run") as run, patch("urllib.request.urlopen", side_effect=responses), patch("time.sleep"):
                run.return_value.returncode = status
                runpy.run_path(str(uploader))
                self.assertEqual(any(call.args[0][1] == "publish" for call in run.call_args_list), should_publish)
        mismatch = io.BytesIO(b'{"version":{"checksum":"different"}}')
        with patch("subprocess.run") as run, patch("urllib.request.urlopen", return_value=mismatch):
            with self.assertRaises(ValueError):
                runpy.run_path(str(uploader))
            self.assertEqual(run.call_count, 1)


if __name__ == "__main__":
    unittest.main()

"""Reconcile immutable crates.io versions before and after upload."""
import hashlib
import json
import subprocess
import time
import urllib.error
import urllib.request

import release

package = release.tomllib.loads((release.ROOT / "Cargo.toml").read_text())["package"]
subprocess.run(["cargo", "package", "--locked"], check=True)
archive = release.ROOT / "target/package" / f'{package["name"]}-{package["version"]}.crate'
checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
url = f'https://crates.io/api/v1/crates/{package["name"]}/{package["version"]}'


def existing():
    for attempt in range(3):
        try:
            request = urllib.request.Request(url, headers={"User-Agent": "agy-delegate-rs release automation"})
            with urllib.request.urlopen(request, timeout=30) as response:
                actual = json.load(response)["version"]["checksum"]
            if actual != checksum:
                raise ValueError("published crate differs from this artifact")
            return True
        except urllib.error.HTTPError as error:
            if error.code == 404:
                return False
            if error.code < 500 and error.code != 429:
                raise
        except (urllib.error.URLError, TimeoutError):
            pass
        if attempt < 2:
            time.sleep(2 ** attempt)
    raise RuntimeError("cannot safely query crates.io; publication aborted")


if not existing():
    result = subprocess.run(["cargo", "publish", "--locked", "--no-verify"])
    if result.returncode:
        for attempt in range(3):
            time.sleep(2 ** attempt)
            if existing():
                break
        else:
            raise RuntimeError("crate publication failed; retry the same job")

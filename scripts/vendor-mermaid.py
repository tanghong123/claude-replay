#!/usr/bin/env python3
"""Pin Mermaid's public browser release without Node or build-time downloads."""

import argparse
import base64
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import tarfile
import urllib.request


def download(url: str) -> bytes:
    with urllib.request.urlopen(url, timeout=60) as response:
        return response.read()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="Exact public Mermaid version, such as 12.1.0")
    args = parser.parse_args()
    if not re.fullmatch(r"\d+\.\d+\.\d+", args.version):
        parser.error("version must contain three numeric components")

    metadata = json.loads(download(f"https://registry.npmjs.org/mermaid/{args.version}"))
    archive = download(metadata["dist"]["tarball"])
    algorithm, expected = metadata["dist"]["integrity"].split("-", 1)
    actual = base64.b64encode(hashlib.new(algorithm, archive).digest()).decode()
    if actual != expected:
        raise ValueError(f"Mermaid {args.version}: archive integrity mismatch")

    # Select exact members, never extract an archive-provided path to the filesystem.
    files = {}
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as tar:
        for name, member in (("mermaid.min.js", "dist/mermaid.min.js"), ("LICENSE", "LICENSE")):
            source = tar.extractfile(f"package/{member}")
            if source is None:
                raise ValueError(f"Mermaid {args.version}: missing {member}")
            files[name] = source.read()

    # Deterministic derived assets: keep upstream files untouched for provenance checks.
    files["mermaid.min.js.gz"] = gzip.compress(files["mermaid.min.js"], compresslevel=9, mtime=0)
    files["runtime-path.txt"] = f"/mermaid/{args.version}.js".encode()

    root = Path(__file__).resolve().parent.parent / "vendor" / "mermaid"
    root.mkdir(parents=True, exist_ok=True)
    for name, data in files.items():
        (root / name).write_bytes(data)
    (root / "release.sha256").write_text("".join(
        f"{hashlib.sha256(data).hexdigest()}  {name}\n" for name, data in files.items()
    ))
    (root / "README.md").write_text(
        "# Mermaid browser runtime\n\n"
        f"Pinned version: **{args.version}**. License: MIT (see `LICENSE`).\n\n"
        f"Source: {metadata['dist']['tarball']}\n\n"
        f"Archive integrity: `{metadata['dist']['integrity']}`.\n\n"
        "`mermaid.min.js` and `LICENSE` are unmodified public npm release files. "
        "The HTML crate embeds one gzip copy, with deterministic `mtime=0`, and serves it at "
        f"`/mermaid/{args.version}.js` with immutable caching. "
        "`mermaid.min.js.gz` and `runtime-path.txt` are generated from that pinned release. "
        "The transcript loads it only when a Mermaid fence is mounted; offline exports "
        "carry the same runtime. No mdrev viewer, CDN, Node runtime, or build-time "
        "download is required to render a diagram.\n\n"
        "Update only with `python3 scripts/vendor-mermaid.py <version>`, then run "
        "the checksum test and Mermaid browser cases. Never edit the release files.\n"
    )
    print(f"Pinned Mermaid {args.version}: {len(files['mermaid.min.js'])} browser bytes")


if __name__ == "__main__":
    main()

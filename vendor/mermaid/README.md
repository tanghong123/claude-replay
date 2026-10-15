# Mermaid browser runtime

Pinned version: **12.1.0**. License: MIT (see `LICENSE`).

Source: https://registry.npmjs.org/mermaid/-/mermaid-12.1.0.tgz

Archive integrity: `sha512-wlVCp+8eTupfCeeFvoZNNiTuHrvag0P2jz/ILgb/f/6jkVokUefOcujefi8qUe/j2asHiSePncVsz/xzzA80LQ==`.

`mermaid.min.js` and `LICENSE` are unmodified public npm release files. The HTML crate embeds one gzip copy, with deterministic `mtime=0`, and serves it at `/mermaid/12.1.0.js` with immutable caching. `mermaid.min.js.gz` and `runtime-path.txt` are generated from that pinned release. The transcript loads it only when a Mermaid fence is mounted; offline exports carry the same runtime. No mdrev viewer, CDN, Node runtime, or build-time download is required to render a diagram.

Update only with `python3 scripts/vendor-mermaid.py <version>`, then run the checksum test and Mermaid browser cases. Never edit the release files.

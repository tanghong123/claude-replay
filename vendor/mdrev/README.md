# mdrev, pinned

The monitors' Markdown viewer (#270) is mdrev's embedded viewer, and this crate is the one copy of
mdrev they have (#274). It holds the embedding kit of ONE public mdrev release, vendored under
`release/`, and `agent-monitor` and `agent-monitor-v2` build it into their binaries. Nothing
installed on the machine is looked for: this directory is the dependency, the way `claude-monitor`
depends on the other crates in this repository. `design/mdrev-in-the-preview-pane.md` covers what
the monitor does with it.

| | |
| --- | --- |
| release | mdrev 1.1.13 |
| source | <https://github.com/tanghong123/homebrew-tap/releases/download/mdrev-1.1.13/mdrev-embed-1.1.13.tar.gz> |
| sha256 | `c4c56e435433ba88839dd91865dc7c6cf9eb9902ef8d3446d051fc493c9dbeb2` (the release's published digest) |
| vendored | 2026-09-26 |

The source is the release's `mdrev-embed-<version>.tar.gz`, which mdrev's release notes call "the
kit alone", checked against the sha256 digest the release publishes for it. The application
tarball beside it carries the same kit: for 1.1.12 its `bundle/`, `mdrev-cli.js`, `package.json`
and docs matched this tree byte for byte, and so did the corp tap's `mdrev-embed` kit, apart from
that edition's install instructions. mdrev is MIT-licensed (its formula). Third-party parts of the
bundle, such as its fonts, keep the licences the release ships them under.

## What is here

`release/` is the part of that tarball a host uses, unmodified:

- `bundle/` — the guest: `mdrev.js`, `mdrev.css`, the lazy `chunks/` and `assets/`. The monitor
  serves it from memory at `/mdrev/<version>/`.
- `mdrev-cli.js` and `package.json` — the command line that holds the review notes and lists a
  document's revisions, following renames. The monitor writes them into its scratch directory and
  runs them with node (20 or later). Without node the notes are off, and a local file's history
  comes from git instead, without following renames.
- `docs/*.md` — mdrev's embedding guide and its HTTP contract, pinned beside the code they
  describe, so that a bump's `git diff release/docs/contract.md` shows what the host now has to
  honour.

Left out: the sample host (`mdrev-v2.js` and its `example/` source), the shell launchers
(`mdrev-cli`, `mdrev-v2`), the kit's own README, and the `.html` renderings of the docs.

`release.sha256` lists every file's checksum, and `cargo test -p mdrev` holds the tree to it. A
hand edit fails the test.

## Moving the pin

Moving the pin is explicit and manual, never automatic. The owner, 2026-09-25: "Future upgrades will
be triggered explicitly and manually".

    scripts/vendor-mdrev.sh <version>

The script downloads that release's kit and checks it against the digest the release publishes
(or `--sha256`). It refuses a kit older than 1.1.6, whose guest ignores the options the pane
declares. Then it replaces `release/` whole, rewrites `release.sha256`, this crate's version and
the table above, and runs `cargo check -p mdrev`.

After that, by hand: read the contract's diff, run the gates and the full browser suite (the mdrev
cases, `mdrev-cli conform` included, run against the new pin), and commit the bump as a change of
its own.

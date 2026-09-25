# vendor/

Third-party code this repository builds in, kept here because it cannot be a Cargo dependency.
Today that is one thing: [`mdrev/`](mdrev/), the pinned release of mdrev whose embedded viewer shows
Markdown in the monitors' preview pane (#270, #274). This page covers where it comes from, why it
is vendored rather than fetched, and how it is expected to change. [`mdrev/README.md`](mdrev/README.md)
has the current pin's provenance table and the details of the update script.

## Where mdrev comes from

- **Only mdrev's public release.** The source is the release's embedding kit, "the kit alone" in
  mdrev's release notes:
  `https://github.com/tanghong123/homebrew-tap/releases/download/mdrev-<version>/mdrev-embed-<version>.tar.gz`.
  It is checked against the sha256 digest GitHub publishes for that asset before anything is read
  out of it. The pinned version is in the table in `mdrev/README.md` (1.1.12 as of 2026-09-25).
- **Never mdrev's source repository.** That repository is private and this one is public, so
  nothing from it may enter here: no files, no git dependency, no build step that reads it.
- **Unmodified.** `mdrev/release/` holds the part of the kit a host uses, as released: `bundle/`
  (the guest the monitors serve), `mdrev-cli.js` and `package.json` (the command line that holds
  review notes), and `docs/*.md` (the embedding guide and the HTTP contract the monitor
  implements). `mdrev/release.sha256` lists every file's checksum, and `cargo test -p mdrev` holds
  the tree to it, so a hand edit fails CI.
- **Used by the monitors alone.** Only `claude-monitor` depends on the `mdrev` crate. Both monitor
  binaries serve the bundle from memory at `/mdrev/<version>/` and run the CLI with node 20 or
  later. `agent-replay` carries none of it, and nothing installed on the machine is consulted.

## Why vendored, not fetched at build time

agent-metrics depends on this repository's crates with `git = "…/claude-replay", tag = "v…"`:
Cargo fetches them at build time and `Cargo.lock` pins the commit. mdrev cannot be taken that
way, for these reasons:

- **It is not a crate.** The kit is a JavaScript bundle, a node CLI and documentation, published
  only as a release tarball. Cargo cannot depend on a tarball, and a git dependency on mdrev's
  private repository would fail for anyone else, CI included.
- **A download in `build.rs` would put the network in every clean build.** Builds without network
  access, and CI that cannot reach GitHub, would break. The build would also need its own digest
  check and cache.
- **It is pinned on purpose.** The owner, 2026-09-25: "only depend on static version of mdrev,
  similar to how agent-monitor depends on crates in claude-replay. Future upgrades will be
  triggered explicitly and manually".
- **Extracted files can be reviewed and guarded.** A version bump shows up in `git diff` file by
  file. The pre-push hook and CI's blocked-email job scan those diffs, and a tarball would be
  opaque to both.
- **It costs less in git than the tarball would.** Git compresses each file: the kit takes 2.4 MB
  in the repository's pack, against 2.8 MB for the tarball. A future bump costs roughly what
  changed, because text files are stored as deltas, whereas a new tarball would add a whole new
  opaque blob every time. The price is the checkout: 190 files, 8.6 MB on disk.

## How it is expected to change

**Only by hand, when a person chooses a new mdrev release.** No automatic updates, no floating
version, and no network at build time.

1. `scripts/vendor-mdrev.sh <version>` downloads that release's kit and verifies the published
   digest (or `--sha256`; `--from` takes a tarball already downloaded). It refuses anything older
   than 1.1.6, whose guest ignores the options the pane passes. Then it replaces `mdrev/release/`
   whole, rewrites `release.sha256`, the crate's version and the provenance table, and runs
   `cargo check -p mdrev`.
2. Read `git diff vendor/mdrev/release/docs/contract.md`. That diff is what the monitor's side of
   the HTTP contract now has to honour.
3. Run the gates and the full browser suite. The mdrev cases include `mdrev-cli conform` against a
   live monitor, which is the contract's own definition of done.
4. Commit the bump as a change of its own, then release as usual.

Never edit files under `mdrev/release/`. A fix belongs in mdrev, and arrives here with its next
release.

**If mdrev's releases ever publish the kit as a crate**, from a public repository or a registry,
the monitors can depend on it the way agent-metrics depends on this repository: pinned by tag,
fetched by Cargo. `vendor/mdrev` would then go away, and the checks it runs would move with the
dependency. That change belongs on mdrev's side first, so it is the owner's call.

Anything else vendored here later follows the same rules: a public release only, unmodified,
checksummed and held to the checksums by a test, moved only by a script, and one commit per bump.

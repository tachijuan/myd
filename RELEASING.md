# Releasing

The steps to cut a release, in order. The ordering matters in one place and is
called out where it does.

A release touches three places: the git repository, the GitHub release, and the
Homebrew tap at [`tachijuan/homebrew-myd`](https://github.com/tachijuan/homebrew-myd).
The tap pins a `sha256` of the release tarball, so skipping a step there leaves
`brew install` broken for everyone until it is fixed.

## 1. Bump the version in three files

They must agree. The version is read from `Cargo.toml` at build time, so a
mismatch shows up as `myd --version` disagreeing with the tag.

| File | What to change |
|---|---|
| `myd/Cargo.toml` | `version = "X.Y.Z"` near the top |
| `myd/Cargo.lock` | the `version` under `name = "myd-tui"` |
| `doc/myd.1` | line 1, the `"myd X.Y.Z"` field of `.TH` |

Patch for a bug fix, minor for a feature. The `Cargo.lock` entry can be
regenerated with `cargo update -p myd-tui` rather than edited by hand.

Check the man page still renders:

```bash
man --warnings -E UTF-8 -l doc/myd.1 >/dev/null
```

No output means no warnings.

## 2. Run the tests

```bash
cargo test --locked --manifest-path myd/Cargo.toml
```

The SFTP suite is `#[ignore]`d and needs a live server; everything else runs
headless.

## 3. Commit

The subject line is `Release X.Y.Z: <what changed>`. This project has no
CHANGELOG — the commit messages are the changelog, so the body should explain
what changed and why, not just restate the subject.

Do **not** add `Co-Authored-By:`, `Claude-Session:`, or "Generated with
[Claude Code]" trailers. See `.claude/CLAUDE.md`; these were stripped from the
whole history once already.

## 4. Tag and push

```bash
git tag -a vX.Y.Z -m "Release X.Y.Z: <what changed>"
git push origin master
git push origin vX.Y.Z
```

## 5. Create the GitHub release

```bash
gh release create vX.Y.Z --title "vX.Y.Z" --generate-notes
```

## 6. Get the tarball checksum

**This is the ordering constraint: the tag must exist on GitHub before this
will work, and the formula cannot be updated until you have the number.**

```bash
curl -sL https://github.com/tachijuan/myd/archive/refs/tags/vX.Y.Z.tar.gz | shasum -a 256
```

Take it from the GitHub URL, not from a local `git archive`. GitHub's tarball
has a `myd-X.Y.Z/` top-level prefix; a local one does not, so the checksums
differ and Homebrew will reject the download.

## 7. Update the Homebrew tap

The tap is the clone Homebrew already keeps:

```bash
cd "$(brew --repository tachijuan/myd)"
```

**Set the git identity first — it will be missing.** Step 9 untaps and
re-installs to verify the release from a user's position, and that re-clones
the repo, discarding any local config. So every release starts with a clone
that has no `user.name`, and the commit below fails with "Author identity
unknown". Set it locally rather than globally, matching the existing history:

```bash
git config user.name "Juan Orlandini"
git config user.email "tachijuan@gmail.com"
```

Then edit `Formula/myd.rb`:

- `url` — point at the new tag
- `sha256` — the number from step 6

Leave the `depends_on` lines alone. `openssl@3` and `pkg-config` are
`=> :build` for a non-obvious reason documented in the comment above them:
`ssh2-config` build-depends on `git2`, which pulls in `openssl-sys`, whose
build script needs OpenSSL headers even though nothing links against them.
Removing them breaks the build on a clean machine.

Then verify before pushing:

```bash
brew audit --strict --online tachijuan/myd/myd
brew install --build-from-source tachijuan/myd/myd
brew test tachijuan/myd/myd
```

Commit and push.

## 8. Publish to crates.io

```bash
cargo publish --manifest-path myd/Cargo.toml
```

The crate is **`myd-tui`**, not `myd` — that name was taken by an unrelated
package. The `[lib]` and `[[bin]]` sections both pin the name `myd`, so the
installed command and every `myd::` path in the tests are unaffected. Do not
"fix" the package name back to `myd`; it will fail to publish.

Publishing is irreversible. A version can be yanked, which hides it from new
resolutions, but it can never be deleted and the version number can never be
reused.

## 9. Verify from a user's position

```bash
brew update && brew upgrade myd
myd --version          # => myd X.Y.Z
```

If a clean check is wanted, remove all local tap state first so the install can
only come from GitHub:

```bash
brew uninstall myd && brew untap tachijuan/myd
brew install tachijuan/myd/myd
```

## Notes

- **Bottles are not set up.** `brew install` builds from source (~45s) and pulls
  a Rust toolchain as a build dependency. Prebuilt bottles would need a release
  workflow building a macOS arm64 / macOS x86_64 / Linux matrix.
- **The demo casts under `demo/` are gitignored** and regenerated with
  `demo/record.sh`. If a release changes the UI enough to invalidate them,
  re-recording is its own task — `demo/verify.sh` will fail on a stale
  `expected` table.

# F-Droid and IzzyOnDroid

Nothing here is submitted. Thomas submits (steps at the end). Docs used: f-droid.org Build
Metadata Reference, Reproducible Builds, Inclusion Policy, "All About Descriptions, Graphics, and
Screenshots". The IzzyOnDroid policy page could not be fetched when this was written (its GitLab
project is archived); re-read the current inclusion policy before asking.

## What was prepared

| File | Purpose |
|---|---|
| `packaging/android/build-apk.sh` | reproducible build; `MIRRORWIRE_UNSIGNED=1` writes `MirrorWire-arm64-v8a-unsigned.apk` (aligned, unsigned) |
| `packaging/fdroid/io.github.windwerfer.mirrorwire.yml` | draft fdroiddata `metadata/<id>.yml` |
| `fastlane/metadata/android/en-US/` | title, descriptions, icon, `changelogs/<versionCode>.txt` |
| `rust-toolchain.toml` | the exact compiler (1.100.0) |

## versionCode

`versionCode = major * 1_000_000 + minor * 1_000 + patch` of the workspace version (minor and
patch below 1000): 0.2.2 -> 2002, 0.3.0 -> 3000, 1.0.0 -> 1000000. v0.2.1 shipped with 139 (the
old CI run number), so every new release upgrades it. `build-apk.sh` derives the number from
`Cargo.toml`. `Cargo.toml` also lists it (`[workspace.metadata.android] version_code`) so F-Droid's
`UpdateCheckData` can read it with a regex (that field takes one capture group, it cannot
calculate); the script refuses to build when the two disagree. To release: bump `version` and
`version_code` together, add `fastlane/metadata/android/en-US/changelogs/<code>.txt`.
`VERSION_CODE=n` overrides it for experiments.

The dev profile (`MIRRORWIRE_PROFILE=dev`) is a different package, `io.github.windwerfer.mirrorwire.dev`,
so it can neither replace nor downgrade the release app. Its code is the release code plus
2_000_000_000 (still below the 2_100_000_000 limit), which keeps it above the old timestamp-style
dev builds (about 1_795_000_000), so those still update.

## Reproducible build

Unsigned APK, same source commit, same toolchain: same bytes, whatever the checkout path or
`CARGO_TARGET_DIR`. What pins it:

- Rust 1.100.0 (`rust-toolchain.toml`; CI installs the same), `--locked`, `cargo-ndk` 4.1.2, NDK
  27.2.12479018 (checked by the script), build-tools 35.0.0, platform android-35, JDK 17.
- `--remap-path-prefix` for the checkout, `CARGO_HOME`, the cargo target dir (build scripts such as
  glutin's embed their `OUT_DIR`), the Rust sysroot and the NDK; `-Wl,--build-id=none`.
- `SOURCE_DATE_EPOCH` = commit time of HEAD (override with the variable). It is used for the zip
  entry times.
- Zip entries added in a fixed order with fixed times and attributes; `classes.dex` is stored
  (no zlib version dependence); `javac -g:none`; no baseline profile; no `git describe` or build
  time in the binary (`env!` is only `CARGO_PKG_VERSION`).
- Signing: apksigner v2 + v3, v1 off (minSdk is 26, which has v2; v2/v3 cover the whole file, so
  the unsigned APK must match byte for byte, which is exactly what F-Droid's apksigcopier flow
  checks). `--alignment-preserved true` is required: without it apksigner 35.0.0 re-aligns entries
  and the copied signature no longer verifies.

Check it yourself (needs `pip install apksigcopier` in a venv and `apksigner` on PATH):

```sh
for d in /tmp/rb-a /tmp/rb-b/deeper; do
  git clone -q . $d && (cd $d && CARGO_TARGET_DIR=$d-target MIRRORWIRE_UNSIGNED=1 packaging/android/build-apk.sh $d-out)
done
sha256sum /tmp/rb-a-out/*.apk /tmp/rb-b/deeper-out/*.apk      # must match
apksigcopier compare MirrorWire-arm64-v8a.apk --unsigned /tmp/rb-a-out/MirrorWire-arm64-v8a-unsigned.apk
```

CI (`build.yml`, Android job) rebuilds unsigned in a different target directory and runs
`apksigcopier compare` against the signed release APK on every build.

## Source and dependency audit (F-Droid inclusion)

- Everything builds from source; the APK path has no vendored binaries (the only binary in the
  tree is the icon PNG) and no proprietary or Google dependency.
- Git dependencies: only `storytold/craft-libs` (public, Apache-2.0), pinned by commit
  (`068e079c...`) in `Cargo.toml` and `Cargo.lock`. Crates come from crates.io via the lockfile.
- Network: the app talks only to the presenter the user pairs with, on the LAN. No ads, tracking
  or non-free service, so no anti-features are claimed.
- F-Droid's build server must offer platform android-35 and build-tools 35.0.0 and allow network
  for `cargo` (crates.io) and rustup during the build; reviewers decide. If not, fall back to the
  IzzyOnDroid route below.
- Licence: source is `MIT OR Apache-2.0`; F-Droid's `License:` takes one SPDX id, the draft uses `MIT`.

## Checklist for Thomas

1. Add 2-8 real screenshots to `fastlane/metadata/android/en-US/images/phoneScreenshots/`.
2. ~~Release v0.2.2~~ done: signed with the release key, `MirrorWire-arm64-v8a.apk` attached.
3. ~~Release certificate hash~~ done: `AllowedAPKSigningKeys` in the yml is the SHA-256
   `4757b5b651b55f9f8e74f0f1202f0f367a0d16d2d74c46a9bf801c9bee5acaca` (CN=Thomas Barth, v2 + v3 signed),
   read with `apksigner verify --print-certs` from the published v0.2.2 APK. A local build at the tag
   passed `apksigcopier compare` against that APK.
4. Fork <https://gitlab.com/fdroid/fdroiddata>, copy the yml to `metadata/io.github.windwerfer.mirrorwire.yml`,
   run `fdroid lint`, `fdroid rewritemeta` and `fdroid build -v -l io.github.windwerfer.mirrorwire`
   (fdroidserver docs), open the merge request; the checkmr bot and reviewers comment there.
   Because the release signature will be copied, run `fdroid signatures` on the release APK if asked.
5. IzzyOnDroid: open an inclusion request at the channel its current policy names, with the repo
   URL and the release APK link. It takes APKs from GitHub releases, wants a free licence, no
   unlisted anti-features, and likes fastlane metadata and a reproducible build (all done here);
   confirm the details in its policy first.

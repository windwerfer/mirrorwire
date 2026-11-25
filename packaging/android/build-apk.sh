#!/usr/bin/env bash
# Build a debug-signed arm64-v8a APK of the Android viewer without Gradle:
# cargo-ndk (Rust .so) + javac/d8 (thin Java shell) + aapt2 + zipalign + apksigner.
#
# Needs: python3, ANDROID_HOME (platforms;android-35, build-tools;35.0.0), ANDROID_NDK_HOME,
# a JDK 17 on PATH, cargo-ndk and the aarch64-linux-android Rust target.
# Usage: packaging/android/build-apk.sh [out-dir]
# Optional: ANDROID_ABI (default arm64-v8a; x86_64 for the emulator),
#           MIRRORWIRE_KEYSTORE=path (a debug key is generated when missing),
#           MIRRORWIRE_KS_PASS / MIRRORWIRE_KEY_PASS / MIRRORWIRE_KEY_ALIAS for a
#           release key (default: the debug key's "android" / androiddebugkey),
#           VERSION_CODE (default: major*1000000 + minor*1000 + patch of the Cargo version,
#             e.g. 0.2.2 -> 2002; the dev profile adds 2000000000, see packaging/fdroid/README.md),
#           MIRRORWIRE_UNSIGNED=1 (stop after zipalign and write the unsigned, aligned
#             MirrorWire-<abi>-unsigned.apk that F-Droid's recipe takes; needs no key),
#           SOURCE_DATE_EPOCH (default: the commit time of HEAD; fixes every timestamp),
#           MIRRORWIRE_PROFILE=release|dev (default release; dev = cargo's unoptimised dev
#           profile, faster to build for local testing, installed next to the release app as
#           its own app: id io.github.windwerfer.mirrorwire.dev, label "MirrorWire Dev",
#           file MirrorWire-dev-<abi>.apk).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="$(mkdir -p "${1:-dist}" && cd "${1:-dist}" && pwd)"
HERE="$ROOT/packaging/android"
PLATFORM="${ANDROID_PLATFORM:-android-35}"
BUILD_TOOLS="${ANDROID_BUILD_TOOLS:-35.0.0}"
ABI="${ANDROID_ABI:-arm64-v8a}"
: "${ANDROID_HOME:?set ANDROID_HOME to the Android SDK}"
: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to the Android NDK}"
BT="$ANDROID_HOME/build-tools/$BUILD_TOOLS"
ANDROID_JAR="$ANDROID_HOME/platforms/$PLATFORM/android.jar"
for f in "$BT/aapt2" "$BT/d8" "$BT/zipalign" "$BT/apksigner" "$ANDROID_JAR"; do
  [ -e "$f" ] || { echo "missing $f" >&2; exit 1; }
done

# Reproducible build (packaging/fdroid/README.md): same inputs give the same unsigned APK bytes.
NDK_REV="27.2.12479018"
if [ "$(sed -n 's/^Pkg.Revision *= *//p' "$ANDROID_NDK_HOME/source.properties" 2>/dev/null)" != "$NDK_REV" ] \
  && [ "${MIRRORWIRE_PROFILE:-release}" = release ]; then
  echo "release builds need NDK $NDK_REV (ANDROID_NDK_HOME=$ANDROID_NDK_HOME)" >&2; exit 1
fi
if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
  SOURCE_DATE_EPOCH="$(git -C "$ROOT" log -1 --format=%ct 2>/dev/null)" \
    || { echo "set SOURCE_DATE_EPOCH (no git history to take the commit time from)" >&2; exit 1; }
fi
export SOURCE_DATE_EPOCH

VERSION_NAME="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)"
# versionCode = major*1_000_000 + minor*1_000 + patch (0.2.2 -> 2002); minor and patch < 1000.
if [ -z "${VERSION_CODE:-}" ]; then
  IFS=. read -r V_MAJOR V_MINOR V_PATCH <<<"${VERSION_NAME%%[-+]*}"
  if [[ ! "$V_MAJOR.$V_MINOR.$V_PATCH" =~ ^[0-9]+\.[0-9]{1,3}\.[0-9]{1,3}$ ]]; then
    echo "cannot derive a versionCode from version \"$VERSION_NAME\"" >&2; exit 1
  fi
  VERSION_CODE=$((10#$V_MAJOR * 1000000 + 10#$V_MINOR * 1000 + 10#$V_PATCH))
  LISTED_CODE="$(sed -n 's/^version_code *= *\([0-9]*\).*/\1/p' "$ROOT/Cargo.toml" | head -n1)"
  [ "$LISTED_CODE" = "$VERSION_CODE" ] \
    || { echo "Cargo.toml [workspace.metadata.android] version_code is '$LISTED_CODE', the version gives $VERSION_CODE" >&2; exit 1; }
  # The dev app is its own package, so it cannot downgrade the release app; the offset keeps it
  # above the old timestamp-style dev codes (about 1.79e9) so updating over those still works.
  [ "${MIRRORWIRE_PROFILE:-release}" = dev ] && VERSION_CODE=$((VERSION_CODE + 2000000000))
fi
# The Java class stays io.github.windwerfer.mirrorwire.MainActivity in both; only the dev app
# id is renamed (aapt2 --rename-manifest-package keeps component names pointing at it).
case "${MIRRORWIRE_PROFILE:-release}" in
  release) PROFILE_ARGS=(--release) RENAME_ARGS=() LABEL=MirrorWire APK_NAME="MirrorWire-$ABI.apk" ;;
  dev)
    PROFILE_ARGS=() RENAME_ARGS=(--rename-manifest-package io.github.windwerfer.mirrorwire.dev)
    LABEL="MirrorWire Dev" APK_NAME="MirrorWire-dev-$ABI.apk"
    ;;
  *) echo "MIRRORWIRE_PROFILE must be release or dev, not ${MIRRORWIRE_PROFILE}" >&2; exit 1 ;;
esac
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "== Rust library ($ABI, ${MIRRORWIRE_PROFILE:-release} profile)"
# Reproducible Rust build: no absolute build paths in the binary, no build id, same flags
# whatever the checkout path. CARGO_ENCODED_RUSTFLAGS is what cargo-ndk appends its own to.
CARGO_HOME_DIR="${CARGO_HOME:-$HOME/.cargo}"
SYSROOT="$(rustc --print sysroot)"
RUSTFLAGS_REPRO=("--remap-path-prefix=$ROOT=/mirrorwire" "--remap-path-prefix=$CARGO_HOME_DIR=/cargo"
  "--remap-path-prefix=${CARGO_TARGET_DIR:-$ROOT/target}=/target" "--remap-path-prefix=$SYSROOT=/rustc-sysroot" "--remap-path-prefix=$ANDROID_NDK_HOME=/ndk"
  "-Clink-arg=-Wl,--build-id=none")
export CARGO_ENCODED_RUSTFLAGS="$(IFS=$'\x1f'; echo "${RUSTFLAGS_REPRO[*]}")"
unset RUSTFLAGS
export CARGO_INCREMENTAL=0
(cd "$ROOT" && cargo ndk -t "$ABI" --platform 26 -o "$WORK/apk/lib" build "${PROFILE_ARGS[@]}" --locked -p mirrorwire-android)
SO="$WORK/apk/lib/$ABI/libmirrorwire_android.so"
[ -f "$SO" ] || { echo "cargo-ndk did not produce $SO" >&2; exit 1; }
"$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/llvm-strip --strip-debug "$SO"
ls -la "$WORK/apk/lib/$ABI"

echo "== Java shell"
mkdir -p "$WORK/classes" "$WORK/dex"
javac -source 11 -target 11 -Xlint:-options -encoding UTF-8 -g:none -classpath "$ANDROID_JAR" \
  -d "$WORK/classes" $(find "$HERE/java" -name '*.java')
"$BT/d8" --release --min-api 26 --lib "$ANDROID_JAR" --output "$WORK/dex" $(find "$WORK/classes" -name '*.class')

echo "== Resources and manifest"
mkdir -p "$WORK/res/mipmap-xxxhdpi"
cp "$ROOT/packaging/icons/io.github.windwerfer.MirrorWire.png" "$WORK/res/mipmap-xxxhdpi/ic_launcher.png"
"$BT/aapt2" compile --dir "$WORK/res" -o "$WORK/res.zip"
sed -e "s/@VERSION_CODE@/$VERSION_CODE/" -e "s/@VERSION_NAME@/$VERSION_NAME/" \
  -e "s/android:label=\"MirrorWire\"/android:label=\"$LABEL\"/" \
  "$HERE/AndroidManifest.xml" > "$WORK/AndroidManifest.xml"
grep -q "android:label=\"$LABEL\"" "$WORK/AndroidManifest.xml" \
  || { echo "could not set the app label in AndroidManifest.xml" >&2; exit 1; }
"$BT/aapt2" link -o "$WORK/unsigned.apk" -I "$ANDROID_JAR" "${RENAME_ARGS[@]}" \
  --manifest "$WORK/AndroidManifest.xml" "$WORK/res.zip"

echo "== Package"
cp "$WORK/dex/classes.dex" "$WORK/apk/"
# Native libs stay uncompressed (extractNativeLibs=false) and 16 KiB aligned.
# Fixed entry order, timestamps and attributes (the Python default would use file mtimes).
# classes.dex is stored, not deflated, so the bytes do not depend on the zlib version.
python3 - "$WORK/unsigned.apk" "$WORK/apk" <<'PY'
import os, sys, time, zipfile
apk, root = sys.argv[1], sys.argv[2]
epoch = max(int(os.environ["SOURCE_DATE_EPOCH"]), 315532800)  # zip cannot go below 1980
stamp = time.gmtime(epoch)[:6]

def add(z, full, name):
    info = zipfile.ZipInfo(name, stamp)
    info.compress_type = zipfile.ZIP_STORED
    info.create_system = 0
    info.external_attr = 0
    with open(full, "rb") as f:
        z.writestr(info, f.read())

with zipfile.ZipFile(apk, "a") as z:
    add(z, os.path.join(root, "classes.dex"), "classes.dex")
    libs = []
    for d, _, files in os.walk(os.path.join(root, "lib")):
        libs += [os.path.join(d, f) for f in files]
    for full in sorted(libs):
        add(z, full, os.path.relpath(full, root))
PY
"$BT/zipalign" -P 16 -f 4 "$WORK/unsigned.apk" "$WORK/aligned.apk"

if [ "${MIRRORWIRE_UNSIGNED:-0}" = 1 ]; then
  APK="$OUT/${APK_NAME%.apk}-unsigned.apk"
  cp "$WORK/aligned.apk" "$APK"
  echo "unsigned, aligned APK (SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH, versionCode $VERSION_CODE):"
  ls -la "$APK"; sha256sum "$APK"
  exit 0
fi

KEYSTORE="${MIRRORWIRE_KEYSTORE:-$HOME/.android/mirrorwire-debug.keystore}"
mkdir -p "$(dirname "$KEYSTORE")"
if [ ! -f "$KEYSTORE" ]; then
  echo "== Generating debug key $KEYSTORE"
  keytool -genkeypair -v -keystore "$KEYSTORE" -storepass android -keypass android \
    -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 \
    -dname "CN=MirrorWire Debug,O=MirrorWire,C=DE" >/dev/null
fi
APK="$OUT/$APK_NAME"
# Passwords go through env: so they never show up in the process list or logs.
export MIRRORWIRE_KS_PASS="${MIRRORWIRE_KS_PASS:-android}"
export MIRRORWIRE_KEY_PASS="${MIRRORWIRE_KEY_PASS:-$MIRRORWIRE_KS_PASS}"
"$BT/apksigner" sign --ks "$KEYSTORE" --ks-pass env:MIRRORWIRE_KS_PASS \
  --key-pass env:MIRRORWIRE_KEY_PASS --ks-key-alias "${MIRRORWIRE_KEY_ALIAS:-androiddebugkey}" \
  --v1-signing-enabled false --v2-signing-enabled true --v3-signing-enabled true \
  --out "$APK" "$WORK/aligned.apk"
"$BT/apksigner" verify --verbose "$APK" | head -n 4
ls -la "$APK"

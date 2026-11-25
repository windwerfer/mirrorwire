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
#           VERSION_CODE (default 1),
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

VERSION_NAME="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)"
VERSION_CODE="${VERSION_CODE:-1}"
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
(cd "$ROOT" && cargo ndk -t "$ABI" --platform 26 -o "$WORK/apk/lib" build "${PROFILE_ARGS[@]}" --locked -p mirrorwire-android)
SO="$WORK/apk/lib/$ABI/libmirrorwire_android.so"
[ -f "$SO" ] || { echo "cargo-ndk did not produce $SO" >&2; exit 1; }
"$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/llvm-strip --strip-debug "$SO"
ls -la "$WORK/apk/lib/$ABI"

echo "== Java shell"
mkdir -p "$WORK/classes" "$WORK/dex"
javac -source 11 -target 11 -Xlint:-options -encoding UTF-8 -classpath "$ANDROID_JAR" \
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
python3 - "$WORK/unsigned.apk" "$WORK/apk" <<'PY'
import os, sys, zipfile
apk, root = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(apk, "a") as z:
    z.write(os.path.join(root, "classes.dex"), "classes.dex", zipfile.ZIP_DEFLATED)
    for d, _, files in os.walk(os.path.join(root, "lib")):
        for f in sorted(files):
            full = os.path.join(d, f)
            z.write(full, os.path.relpath(full, root), zipfile.ZIP_STORED)
PY
"$BT/zipalign" -P 16 -f 4 "$WORK/unsigned.apk" "$WORK/aligned.apk"

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
  --out "$APK" "$WORK/aligned.apk"
"$BT/apksigner" verify --verbose "$APK" | head -n 4
ls -la "$APK"

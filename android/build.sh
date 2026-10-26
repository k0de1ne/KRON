#!/usr/bin/env bash
# Build kron.apk from an aarch64-linux-android kron binary, without Gradle:
#   android/build.sh target/aarch64-linux-android/release/kron kron.apk [llama-server]
# With llama-server (llama.cpp built for Android) the app can run local models.
# Needs ANDROID_HOME with platforms;android-35 and build-tools;35.0.0, and a JDK.
# Signs with $KEYSTORE / $KEYSTORE_PASS if set, else with android/debug.keystore.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
bin=$(realpath "$1")
out=$(realpath -m "$2")
llama=${3:+$(realpath "$3")}
bt="$ANDROID_HOME/build-tools/35.0.0"
jar="$ANDROID_HOME/platforms/android-35/android.jar"
b="$here/build"
rm -rf "$b" && mkdir -p "$b/gen" "$b/classes" "$b/dex" "$b/apk/lib/arm64-v8a"

"$bt/aapt2" compile --dir "$here/res" -o "$b/res.zip"
"$bt/aapt2" link -I "$jar" --manifest "$here/AndroidManifest.xml" -o "$b/base.apk" "$b/res.zip" \
  --java "$b/gen" --min-sdk-version 28 --target-sdk-version 34 \
  --version-code "${VERSION_CODE:-1}" --version-name "${VERSION_NAME:-dev}"
javac -nowarn --release 11 -classpath "$jar" -d "$b/classes" \
  $(find "$here/src" "$b/gen" -name '*.java')
"$bt/d8" --lib "$jar" --min-api 28 --release --output "$b/dex" $(find "$b/classes" -name '*.class')

# The binary ships as a "native library" so Android extracts it to an executable location.
cp "$b/base.apk" "$b/unsigned.apk"
cp "$b/dex/classes.dex" "$b/apk/"
cp "$bin" "$b/apk/lib/arm64-v8a/libkron.so"
if [ -n "$llama" ]; then cp "$llama" "$b/apk/lib/arm64-v8a/libllama-server.so"; fi
(cd "$b/apk" && zip -qr "$b/unsigned.apk" classes.dex lib)
"$bt/zipalign" -p -f 4 "$b/unsigned.apk" "$b/aligned.apk"
"$bt/apksigner" sign --ks "${KEYSTORE:-$here/debug.keystore}" --ks-pass "pass:${KEYSTORE_PASS:-android}" \
  --out "$out" "$b/aligned.apk"
echo "built $out"

#!/usr/bin/env bash
# Fetch proot and its libraries from the Termux repository, named so the APK
# installer extracts them as native libraries (the only place Android 10+ lets
# an app execute files from):
#   android/proot.sh <out dir>
# Needs curl, dpkg-deb and perl.
set -euo pipefail
out=$(realpath -m "$1")
repo=https://packages.termux.dev/apt/termux-main
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"

curl -fsSL "$repo/dists/stable/main/binary-aarch64/Packages" -o "$work/Packages"
for pkg in proot libtalloc libandroid-shmem; do
  file=$(awk -v p="$pkg" '$1 == "Package:" { hit = ($2 == p) } hit && $1 == "Filename:" { print $2; exit }' "$work/Packages")
  sum=$(awk -v p="$pkg" '$1 == "Package:" { hit = ($2 == p) } hit && $1 == "SHA256:" { print $2; exit }' "$work/Packages")
  curl -fsSL "$repo/$file" -o "$work/$pkg.deb"
  echo "$sum  $work/$pkg.deb" | sha256sum -c --quiet
  dpkg-deb -x "$work/$pkg.deb" "$work/root"
done

usr="$work/root/data/data/com.termux/files/usr"
cp "$usr/bin/proot" "$out/libproot.so"
cp "$usr/libexec/proot/loader" "$out/libproot-loader.so"
cp "$(realpath "$usr/lib/libtalloc.so.2")" "$out/libtalloc.so"
cp "$usr/lib/libandroid-shmem.so" "$out/libandroid-shmem.so"
# The installer only keeps lib*.so, so proot must look for libtalloc.so, not
# libtalloc.so.2. Same-length rewrite of the name in its dynamic string table.
perl -pi -e 's/libtalloc\.so\.2\0/libtalloc.so\0\0\0/g' "$out/libproot.so" "$out/libtalloc.so"
ls -la "$out"

#!/usr/bin/env bash
# Install a built package in a clean throwaway container, then run the
# smoke tests with networking disabled (docker --network none):
#   1. headless engine (smoke-engine.sh): status, notes list, PDF preview,
#      no python in the engine process tree;
#   2. the ms-app sign-in callback through the shell (`<app>
#      __notes-login-callback <synthetic uri>`): forwarded to notes-engine,
#      rejected cleanly (exit 1, fixed message, no window, no state written
#      outside a scratch XDG_STATE_HOME);
#   3. the GUI shell under Xvfb: window process starts, spawns notes-engine,
#      no python anywhere in the container.
# AppImage: libgbm1/libegl1 (Mesa; linuxdeploy never bundles them, every
# desktop has them) are installed in the test image.
#
#   packaging/smoke/container-smoke.sh FORMAT PACKAGE LIBRARY [IMAGE]
#     FORMAT   deb | rpm | appimage | arch
#     LIBRARY  synthetic library root (gen_library.py --out DIR); copied
#     IMAGE    default: deb ubuntu:24.04, appimage ubuntu:22.04,
#              rpm fedora:latest, arch archlinux:latest
set -euo pipefail
format="$1"; package="$(realpath "$2")"; library="$(realpath "$3")"
here="$(cd "$(dirname "$0")" && pwd)"
case "$format" in
  deb)      image="${4:-ubuntu:24.04}"
            install="apt-get update -qq && apt-get install -y -qq /pkg/$(basename "$package") curl procps xvfb xauth >/dev/null"
            engine=/usr/bin/notes-engine; root=/usr/lib/Notes; gui=notes;;
  appimage) image="${4:-ubuntu:22.04}"
            install="apt-get update -qq && apt-get install -y -qq poppler-utils bubblewrap xdg-utils curl procps xvfb xauth squashfs-tools libgbm1 libegl1 >/dev/null && cd /opt && /pkg/$(basename "$package") --appimage-extract >/dev/null && chmod -R a+rX /opt/squashfs-root"
            # No FUSE in the container: the GUI runs from the extracted image (AppRun).
            engine=/opt/squashfs-root/usr/bin/notes-engine; root=/opt/squashfs-root/usr/lib/Notes; gui=/opt/squashfs-root/AppRun;;
  rpm)      image="${4:-fedora:latest}"
            install="dnf install -y -q /pkg/$(basename "$package") curl procps-ng xorg-x11-server-Xvfb xorg-x11-xauth which >/dev/null"
            engine=/usr/bin/notes-engine; root=/usr/lib/Notes; gui=notes;;
  arch)     image="${4:-archlinux:latest}"
            install="pacman -Syu --noconfirm -q >/dev/null && pacman -U --noconfirm /pkg/$(basename "$package") >/dev/null && pacman -S --noconfirm --needed curl procps-ng xorg-server-xvfb xorg-xauth >/dev/null"
            engine=/usr/lib/notes/bin/notes-engine; root=/usr/lib/notes; gui=notes;;
  *) echo "unknown format $format" >&2; exit 2;;
esac
tag="notes-smoke-$format:$(date +%s)"
# World-readable staging copy (the library's own dirs are 0700); the
# container copies it and restores private modes.
stage="$(mktemp -d)"
cp -a "$library" "$stage/lib" && chmod -R a+rX "$stage/lib"
library="$stage/lib"
# SMOKE_DOCKER_ARGS: extra `docker create` options for the install step
# (e.g. a local package mirror/cache mount); split on whitespace.
read -ra extra <<<"${SMOKE_DOCKER_ARGS:-}"
cid="$(docker create "${extra[@]}" -v "$(dirname "$package"):/pkg:ro" "$image" sh -c "$install && useradd -m tester")"
docker start -a "$cid"
docker commit "$cid" "$tag" >/dev/null
docker rm "$cid" >/dev/null

rc=0
docker run --rm --network none -u tester -e SMOKE_IN_CONTAINER=1 \
    -v "$here:/smoke:ro" -v "$library:/library-ro:ro" "$tag" sh -c '
set -u
echo "== $(. /etc/os-release; echo "$PRETTY_NAME") / '"$format"' / network: $(ls /sys/class/net | tr "\n" " ")"
cp -a /library-ro /tmp/lib && chmod -R go-rwx /tmp/lib
/smoke/smoke-engine.sh '"$engine"' '"$root"' /tmp/lib/main 6dfd4bbf-e334-4e80-b37b-93f461f09aa3 0e34b3bb-2a10-4ed8-a414-a3b820aa2c55; rc=$?
echo "== sign-in callback via the shell"
cb=$(mktemp -d); out=$(XDG_STATE_HOME=$cb/state XDG_DATA_HOME=$cb/data timeout 20 '"$gui"' __notes-login-callback "ms-app://s-1-15-2-0-0/?code=synthetic&state=synthetic" 2>&1); code=$?
if [ "$code" = 1 ] && echo "$out" | grep "tamamlanamad" >/dev/null && ! pgrep -x notes >/dev/null && ! pgrep -x notes-engine >/dev/null; then echo "PASS callback forwarded and rejected cleanly (exit 1: $(echo "$out" | tail -1))"; else echo "FAIL callback exit $code: $out"; rc=1; fi
echo "== GUI under Xvfb"
rm -rf /tmp/lib && cp -a /library-ro /tmp/lib && chmod -R go-rwx /tmp/lib
export APPIMAGE_EXTRACT_AND_RUN=1 WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 LINUX_NOTES_OFFLINE=1 LINUX_NOTES_DATA_DIR=/tmp/lib/main
xvfb-run -a -s "-screen 0 1400x900x24" '"$gui"' >/tmp/gui.log 2>&1 &
i=0; while [ $i -lt 40 ] && ! pgrep -x notes-engine >/dev/null; do sleep 0.5; i=$((i+1)); done
sleep 3
if pgrep -x notes-engine >/dev/null; then
  e=$(pgrep -o -x notes-engine); p=$(ps -o ppid= -p "$e" | tr -d " ")
  echo "PASS GUI started notes-engine (pid $e, parent: $(ps -o comm= -p $p)); listening: $(cat /proc/net/tcp | awk "NR>1 && \$4==\"0A\" {print \$2}" | tr "\n" " ")"
else echo "FAIL GUI did not start notes-engine"; rc=1; tail -20 /tmp/gui.log; fi
if ps -eo comm= | grep -i python >/dev/null; then echo "FAIL python process in container"; rc=1; else echo "PASS no python process in container ($(ps -eo comm= | sort -u | tr "\n" " "))"; fi
command -v python3 >/dev/null && echo "INFO python3 installed in image" || echo "INFO python3 not installed in image"
exit "$rc"' || rc=$?
docker rmi -f "$tag" >/dev/null
rm -rf "$stage"
exit "$rc"

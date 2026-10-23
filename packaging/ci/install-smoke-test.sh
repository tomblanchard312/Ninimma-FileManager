#!/usr/bin/env bash
# Ninimma modification: independent project packaging and branding.
# Installs the built Linux packages in clean containers and checks that the
# binary, desktop entry, icons and AppStream metadata land where software
# centres and menus look, that the app runs (--version, then a few seconds
# under a virtual X server), and that removing the package removes them.
#
# After packaging/ci/build-linux-release.sh, from the same directory:
#   packaging/ci/install-smoke-test.sh            # deb, rpm and tarball
#   packaging/ci/install-smoke-test.sh deb        # just one
set -euo pipefail

export DOCKER_HOST=${DOCKER_HOST:-unix:///var/run/docker.sock}
docker info >/dev/null || { echo "Docker isn't reachable at $DOCKER_HOST." >&2; exit 1; }

kinds=("$@")
[[ ${#kinds[@]} -gt 0 ]] || kinds=(deb rpm tarball)
id=io.github.tomblanchard312.ninimma

# The checks, run inside each container: $1 is the install prefix.
# shellcheck disable=SC2016
checks='
set -eu
prefix=$1
fail() { echo "FAIL: $*" >&2; exit 1; }
for f in "$prefix/bin/ninimma" \
         "$prefix/share/applications/ninimma.desktop" \
         "$prefix/share/icons/hicolor/scalable/apps/ninimma.svg" \
         "$prefix/share/icons/hicolor/48x48/apps/ninimma.png" \
         "$prefix/share/icons/hicolor/256x256/apps/ninimma.png" \
         "$prefix/share/metainfo/'$id'.metainfo.xml"; do
    [ -f "$f" ] || fail "missing $f"
    echo "ok   $f"
done
grep -q "^Icon=ninimma$" "$prefix/share/applications/ninimma.desktop" || fail "desktop entry icon"
grep -q "<launchable type=\"desktop-id\">ninimma.desktop</launchable>" \
    "$prefix/share/metainfo/'$id'.metainfo.xml" || fail "metainfo launchable"
"$prefix/bin/ninimma" --version | grep -q "^ninimma [0-9]" || fail "--version"
echo "ok   $("$prefix/bin/ninimma" --version)"
# Starts and keeps running: still up when timeout stops it (status 124).
export LIBGL_ALWAYS_SOFTWARE=1
status=0
xvfb-run -a -s "-screen 0 1280x800x24" timeout 8 "$prefix/bin/ninimma" "$HOME" >/tmp/run.log 2>&1 || status=$?
[ "$status" = 124 ] || { cat /tmp/run.log; fail "exited early with status $status"; }
echo "ok   ran for 8 s under Xvfb"
'

run() { # image, then a shell script
    docker run --rm -i -v "$PWD:/pkg:ro" -e CHECKS="$checks" "$1" bash -s
}

deb() {
    local file
    file=$(ls -t ./ninimma_*_amd64.deb | head -1)
    for image in ubuntu:22.04 debian:12; do
        echo "== $file on $image"
        run "$image" <<EOF
set -eu
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq /pkg/$(basename "$file") xvfb xauth libgl1-mesa-dri >/dev/null
useradd -m tester
su tester -c 'bash -c "\$CHECKS" _ /usr'
apt-get remove -y -qq ninimma >/dev/null
for f in /usr/bin/ninimma /usr/share/metainfo/$id.metainfo.xml; do
    [ ! -e "\$f" ] || { echo "FAIL: \$f left after removal" >&2; exit 1; }
done
echo "ok   removed cleanly"
EOF
    done
}

rpm() {
    local file
    file=$(ls -t ./ninimma-*.x86_64.rpm | head -1)
    echo "== $file on fedora:latest"
    run fedora:latest <<EOF
set -eu
dnf install -y -q /pkg/$(basename "$file") xorg-x11-server-Xvfb xorg-x11-xauth mesa-dri-drivers which >/dev/null
# Fedora ships xvfb-run in the Xvfb package.
useradd -m tester
su tester -c 'bash -c "\$CHECKS" _ /usr'
dnf remove -y -q ninimma >/dev/null
for f in /usr/bin/ninimma /usr/share/metainfo/$id.metainfo.xml; do
    [ ! -e "\$f" ] || { echo "FAIL: \$f left after removal" >&2; exit 1; }
done
echo "ok   removed cleanly"
EOF
}

tarball() {
    echo "== ninimma-linux-x64.tar.gz (install.sh, as a user) on ubuntu:22.04"
    run ubuntu:22.04 <<'EOF'
set -eu
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq xvfb xauth libgl1-mesa-dri libx11-xcb1 libxkbcommon-x11-0 libxcursor1 \
    libxi6 libxrender1 libegl1 libgl1 >/dev/null
useradd -m tester
su tester -c 'set -e
    mkdir ~/dl && tar -C ~/dl -xzf /pkg/ninimma-linux-x64.tar.gz
    ~/dl/install.sh
    bash -c "$CHECKS" _ ~/.local' 2>&1
EOF
}

for kind in "${kinds[@]}"; do
    "$kind"
done
echo "All install smoke tests passed: ${kinds[*]}"

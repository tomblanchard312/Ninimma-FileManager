#!/bin/sh
# Runs only inside the disposable FreeBSD build VM.
set -eu
pkg add target/freebsd/*.pkg
/usr/local/bin/ninimma --version | grep '^ninimma '
for file in /usr/local/share/applications/ninimma.desktop /usr/local/share/icons/hicolor/256x256/apps/ninimma.png /usr/local/share/licenses/ninimma/NOTICE; do
    test -s "$file"
done
pw useradd ninimma-test -m
Xvfb :99 -screen 0 1280x800x24 -nolisten tcp -ac >/tmp/ninimma-xvfb.log 2>&1 &
xpid=$!
trap 'kill "$xpid" 2>/dev/null || true' EXIT
sleep 2
status=0
su -l ninimma-test -c 'env DISPLAY=:99 LIBGL_ALWAYS_SOFTWARE=1 timeout 8 /usr/local/bin/ninimma /home/ninimma-test' >/tmp/ninimma-startup.log 2>&1 || status=$?
if [ "$status" != 124 ]; then
    cat /tmp/ninimma-xvfb.log /tmp/ninimma-startup.log
    echo "GUI startup failed: $status" >&2
    exit 1
fi
pkg delete -y ninimma
test ! -e /usr/local/bin/ninimma
test ! -e /usr/local/share/applications/ninimma.desktop
echo 'FreeBSD package install, ordinary-user GUI startup, and removal passed'

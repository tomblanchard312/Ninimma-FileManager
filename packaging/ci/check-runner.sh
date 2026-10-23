#!/usr/bin/env bash
# Ninimma modification: independent project packaging and branding.
# Fail early with an actionable message, before downloading/building gigabytes.
set -euo pipefail
mode=${1:-build}
minimum=${RUNNER_MIN_FREE_GB:-20}
if [[ ! $minimum =~ ^[0-9]+$ ]] || (( minimum < 1 )); then
    echo 'RUNNER_MIN_FREE_GB must be a positive integer.' >&2
    exit 1
fi
require() {
    command -v "$1" >/dev/null || { echo "Missing runner prerequisite: $1" >&2; exit 1; }
}
require git
available=$(df -Pk "${GITHUB_WORKSPACE:-.}" | awk 'NR == 2 {print $4}')
if [[ ! $available =~ ^[0-9]+$ ]]; then
    echo 'Could not determine free workspace disk space.' >&2
    exit 1
fi
echo "Workspace free space: $((available / 1024 / 1024)) GiB; required: $minimum GiB"
if (( available < minimum * 1024 * 1024 )); then
    echo "Insufficient workspace disk space. Free at least $minimum GiB before retrying." >&2
    exit 1
fi
if [[ $mode == container ]]; then
    # The Linux release is built in an Ubuntu 22.04 container: only Docker
    # (the system daemon) is needed on the host.
    require docker
    if ! DOCKER_HOST=${DOCKER_HOST:-unix:///var/run/docker.sock} docker info >/dev/null 2>&1; then
        echo 'Docker is not reachable at /var/run/docker.sock. Start it and add the runner account to the docker group.' >&2
        exit 1
    fi
    exit 0
fi
case ${RUNNER_OS:-Linux} in
    Linux)
        require python3
        require sudo
        require apt-get
        if ! sudo -n true; then
            echo 'Runner requires noninteractive sudo. Configure its service account on the host; CI cannot answer password prompts.' >&2
            exit 1
        fi
        if [[ $mode == vm ]]; then
            require timeout
            require ssh
            require scp
            if [[ ! -c /dev/kvm ]]; then
                echo 'FreeBSD VM builds require /dev/kvm. Enable virtualization and expose KVM to this runner host.' >&2
                exit 1
            fi
        elif [[ $mode == release ]]; then
            # A custom label alone cannot guarantee glibc compatibility.
            version=$(getconf GNU_LIBC_VERSION)
            if [[ $version != 'glibc 2.35' ]]; then
                echo "Linux releases require glibc 2.35 (Ubuntu 22.04); found $version." >&2
                exit 1
            fi
        fi
        ;;
    macOS)
        require xcode-select
        xcode-select -p >/dev/null
        ;;
    Windows)
        require powershell.exe
        powershell.exe -NoProfile -NonInteractive -Command '
          $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
          if (!(Test-Path $vswhere)) { Write-Error "Install Visual Studio Build Tools with Desktop development with C++ and a Windows SDK."; exit 1 }
          $installed = & $vswhere -latest -products "*" -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
          if (!$installed) { Write-Error "MSVC x64 build tools are missing."; exit 1 }
        '
        if [[ $mode == release ]]; then require 7z; fi
        ;;
    *) echo "Unsupported runner OS: ${RUNNER_OS}" >&2; exit 1 ;;
esac

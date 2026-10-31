#!/usr/bin/env bash
# Ninimma modification: independent project packaging and branding.
# Builds the Windows release (ninimma-windows-x64.zip) on Linux,
# cross-compiling with MinGW-w64 inside Ubuntu 24.04, so no Windows machine
# or GitHub-hosted Windows runner is needed.
#
# On the host:   packaging/ci/build-windows-release.sh v0.4.0
# It starts the container itself (against the system Docker daemon) and
# leaves the zip in the current directory, owned by the calling user.
set -euo pipefail

tag=${1:-${GITHUB_REF_NAME:-}}
[[ -n $tag ]] || { echo "usage: $0 <tag, e.g. v0.4.0>" >&2; exit 1; }

if [[ ${IN_RELEASE_CONTAINER:-} != 1 ]]; then
    export DOCKER_HOST=${DOCKER_HOST:-unix:///var/run/docker.sock}
    docker info >/dev/null || { echo "Docker isn't reachable at $DOCKER_HOST." >&2; exit 1; }
    exec docker run --rm \
        -v "$PWD:/src" -w /src \
        -e IN_RELEASE_CONTAINER=1 -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
        ubuntu:24.04 bash packaging/ci/build-windows-release.sh "$tag"
fi

# --- Inside the container, as root ---
target=x86_64-pc-windows-gnu
archive=ninimma-windows-x64.zip
# patch-llama.sh points Cargo.lock at a patched copy: restore it after.
cp Cargo.lock /tmp/Cargo.lock.orig
trap 'cp /tmp/Cargo.lock.orig /src/Cargo.lock; chown -R "$HOST_UID:$HOST_GID" /src/target /src/Cargo.lock /src/$archive 2>/dev/null || true' EXIT

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
    ca-certificates curl git build-essential pkg-config cmake clang libclang-dev \
    perl make zip mingw-w64 wine64 >/dev/null
# llama.cpp needs C++ threads: the POSIX-thread flavour of MinGW.
update-alternatives --set x86_64-w64-mingw32-gcc /usr/bin/x86_64-w64-mingw32-gcc-posix >/dev/null
update-alternatives --set x86_64-w64-mingw32-g++ /usr/bin/x86_64-w64-mingw32-g++-posix >/dev/null
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --target "$target" >/dev/null
. "$HOME/.cargo/env"

# The C/C++ parts (SQLCipher's OpenSSL, llama.cpp) build with MinGW too.
export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
export CXX_x86_64_pc_windows_gnu=x86_64-w64-mingw32-g++
export AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
# bindgen reads the Windows headers, not the host's.
sysroot=/usr/x86_64-w64-mingw32
# (clang's own built-in headers, not MinGW GCC's: mixing them redefines
# max_align_t.)
export BINDGEN_EXTRA_CLANG_ARGS="--target=x86_64-w64-mingw32 --sysroot=$sysroot -I$sysroot/include"
# Ubuntu's MinGW headers predate the thread power-throttling API that
# llama.cpp uses to keep Windows 11 from parking its threads; these are the
# Windows SDK's definitions, for llama.cpp's C files only.
cat > /tmp/mingw-compat.h <<'HEADER'
#include <windows.h>
#ifndef THREAD_POWER_THROTTLING_CURRENT_VERSION
typedef struct _THREAD_POWER_THROTTLING_STATE {
    ULONG Version;
    ULONG ControlMask;
    ULONG StateMask;
} THREAD_POWER_THROTTLING_STATE;
#define THREAD_POWER_THROTTLING_CURRENT_VERSION 1
#define THREAD_POWER_THROTTLING_EXECUTION_SPEED 0x1
#endif
HEADER
# CMake (llama.cpp) cross-compiles to Windows.
cat > /tmp/mingw.cmake <<'EOF'
# Appended to the flags the cmake crate passes (an _INIT value is ignored
# once CMAKE_C_FLAGS is given on the command line).
set(CMAKE_C_FLAGS "${CMAKE_C_FLAGS} -include /tmp/mingw-compat.h")
set(CMAKE_SYSTEM_NAME Windows)
set(CMAKE_SYSTEM_PROCESSOR x86_64)
set(CMAKE_C_COMPILER x86_64-w64-mingw32-gcc)
set(CMAKE_CXX_COMPILER x86_64-w64-mingw32-g++)
set(CMAKE_RC_COMPILER x86_64-w64-mingw32-windres)
set(CMAKE_FIND_ROOT_PATH /usr/x86_64-w64-mingw32)
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
EOF
export CMAKE_TOOLCHAIN_FILE_x86_64_pc_windows_gnu=/tmp/mingw.cmake
# One self-contained .exe: the MinGW C and C++ runtimes linked in.
export RUSTFLAGS="-C target-feature=+crt-static -C link-args=-static-libgcc -C link-args=-static-libstdc++"

# llama.cpp's build script needs a patch for MinGW (see patch-llama.sh).
cfg=$(sh packaging/llama/patch-llama.sh)
cargo build --locked --release --target "$target" --config "$cfg"
exe=target/$target/release/ninimma.exe
release=target/$target/release
python3 packaging/stage-enki.py --target x86_64-pc-windows-msvc --out "$release/enki"

# MinGW's C++ runtime is linked as DLLs (llama.cpp asks for it that way):
# ship the ones the .exe imports next to it, or Windows won't start it.
runtime=()
todo=("$exe")
while ((${#todo[@]})); do
    file=${todo[0]}
    todo=("${todo[@]:1}")
    for dll in $(x86_64-w64-mingw32-objdump -p "$file" | sed -n 's/^\s*DLL Name: //p'); do
        [[ " ${runtime[*]} " == *" $dll "* ]] && continue
        found=$(find /usr/lib/gcc/x86_64-w64-mingw32 /usr/x86_64-w64-mingw32/lib -name "$dll" 2>/dev/null | grep -v win32 | head -n 1 || true)
        if [[ -n $found ]]; then
            # MinGW's runtime (and what it needs in turn); system DLLs aren't found here.
            cp "$found" "$release/"
            runtime+=("$dll")
            todo+=("$found")
        fi
    done
done
echo "Shipped with: ${runtime[*]:-nothing}"

# Starts at all under Wine (no window: --version exits first), with only
# what ships beside it.
WINEDEBUG=-all /usr/lib/wine/wine64 "$exe" --version

cp LICENSE NOTICE "$release/"
cp packaging/icons/LICENSE-lucide "$release/"
rm -f "$archive"
(cd "$release" && zip -qr "../../../$archive" enki ninimma.exe "${runtime[@]}" LICENSE NOTICE LICENSE-lucide)
zip -sf "$archive"

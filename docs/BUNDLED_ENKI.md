# Bundled Enki runtime (review preview)

App packages stage only compiled Enki binaries and license notices from the public
[Enki-runtime releases](https://github.com/tomblanchard312/Enki-runtime/releases).
The private daemon/worker source and Git history are never fetched by app builds.
The separately approved MIT client/protocol/speech SDK remains public.

`packaging/enki-runtime.json` pins the reviewed platform archive and SHA-256.
`python3 packaging/stage-enki.py --target <Rust target> --out <app runtime directory>`
verifies the entire archive before staging. `ENKI_RUNTIME_ARCHIVE` can select a
local copy of that exact archive for offline builds; it cannot bypass the pin.
Only compiled executables/libraries, license notices and runtime metadata are
allowed. Extra files, symlinks, wrong targets and missing notices are rejected.

The SDK finds the installed `enki` directory beside the app or under
`../lib/<app>/enki`, and starts the daemon as the ordinary user. A healthy shared
service is reused and may continue after the app closes. This installs no OS
service, records no microphone audio, downloads no model and runs no privileged
security helper. Recording remains an explicit, bounded app action. Model files
must be supplied separately. A configured ENKI_WHISPER_CLI remains authoritative.

The initial x86_64 Linux archive is a preview built on Ubuntu 24.04/glibc 2.39 and
requires AVX2 for text/Whisper inference. It does not satisfy the apps' existing
Ubuntu 22.04/glibc 2.35 baseline. Packaging rejects an older glibc host. An older
Linux baseline build and native Windows/macOS/FreeBSD/other-architecture archives
are required before those targets can release. Missing artifacts block packaging
rather than silently omit Enki. Private hosted builds are currently blocked before
startup by the GitHub account's billing/spending-limit condition.

Tests: `python3 packaging/test_enki_runtime.py` covers checked binary-only staging,
wrong-platform/checksum rejection, source/traversal rejection, missing license
notices and symlink rejection. Native installation, audio permissions, signing and
assistive-technology behavior still require platform testing. Do not merge this
review preview as a production release acceptance claim.

Known lifecycle release blocker: the shared daemon currently runs from the app
that first started it. Stop Enki before updating or uninstalling that provider
app; otherwise other apps can lose access to its worker files. A shared immutable
runtime install or tested installer shutdown handoff is required before claiming
safe live cross-app updates/uninstalls.

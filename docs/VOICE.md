# Voice, dictation and transcription

Choose **Voice** in the top bar to open the shared Enki speech panel. Model names and registration details are not shown in the normal controls.

- **Dictate** records for at most 30 seconds. Stop early or discard the recording. Review and edit the transcript before using it.
- **Transcribe recording…** opens a recording you explicitly select. Supported formats depend on the installed whisper.cpp build; WAV is the simplest baseline. The daemon caps inputs at 512 MiB and processing at ten minutes.
- **Translate transcript** translates locally into the named language. Whisper handles file-to-English translation; other translations use an available local Phi4 or Qwen text model. Translation can make mistakes; review it.
- **Voice command** listens for five seconds after activation and recognizes one exact supported phrase. There is no always-listening mode. Closing the panel or changing window focus stops microphone capture; closing it cancels pending work. Commands are cancelled on focus loss and are never applied to a changed destination.

Recording has a visible indicator, a stop button and a discard button. Audio goes to the local Enki daemon. No cloud service or model download starts from these controls. Microphone permission is requested by the operating system when capture starts. Keyboard users can focus and activate the regular controls.

## In this app

Reviewed text fills the search field; it does not rename, move or delete files. Say “next” or “previous” to use folder history, “zoom in” or “zoom out” to adjust UI size, or “open assistant” to keep the speech panel open. Undo/redo voice requests explain that file changes need the normal review controls.

## Setup and limits

Use the combined Enki speech/image build linked by this branch, with protocol versions 4 and 5, and install whisper.cpp for your platform. Set `ENKI_WHISPER_CLI` in the daemon's environment to the absolute path of `whisper-cli` if it is not on its PATH. Add an appropriate `ggml-*.bin` Whisper model to the existing Enki models folder (`$XDG_DATA_HOME/SharedAI/models`, normally `~/.local/share/SharedAI/models` on Unix; `%LOCALAPPDATA%/SharedAI/models` on Windows), or register it through the SDK. The scanner waits for files to settle before registering them. The user supplies the model; keep its license and notices. Multilingual models are needed for multilingual recognition; `.en` models recognize English only.

Selection is automatic: prefer a registered small/base/tiny Whisper model and a local Phi4/Qwen text model when translation needs one. Availability and quality depend on the installed model and hardware. Missing prerequisites produce a setup message rather than silently switching to a remote service.

Linux builds need ALSA development headers (`libasound2-dev` on Debian/Ubuntu); the installed app needs the ALSA runtime. FreeBSD builds need `alsa-lib` and a working audio configuration. Where packaged here, macOS bundles declare microphone usage (and hardened-runtime audio input entitlement where used). Flatpak manifests include audio and the per-user Enki socket access; actual runtime microphone routing and selected-file visibility still need packaged testing.

Validation covers synthetic audio conversion, command allowlisting/cancellation, SDK/daemon IPC and native Linux compilation. A real CPU Whisper tiny.en run through Enki transcribed the upstream public JFK fixture, exercised English translation mode and returned a five-second PCM caption; its session was cleaned up. Physical microphones, multilingual recognition/translation quality, accessibility tooling, Windows, macOS, FreeBSD and packaged Flatpak interaction remain release gates. The shared panel currently uses English; hosts translate the top-bar label where available. This is push-to-talk accessibility support, not a promise of full hands-free operation.

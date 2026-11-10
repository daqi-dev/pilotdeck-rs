---
name: audio-transcription
description: Transcribe a project-local audio recording, generate subtitles, analyze a recording, or produce meeting notes from audio using local FunASR.
---

# Audio Transcription

Use this Skill only when the user explicitly asks to transcribe audio, create subtitles, analyze a recording, or make meeting notes from a recording. Do not invoke ASR merely because an audio attachment is present.

PilotDeck provides this Skill itself; there is no Skill file to install. First check whether `mcp__funasr__transcribe_audio` is available. If its local runtime or models are missing, run the exact installation command shown by the attachment or MCP diagnostic.

A Rust source installation ships `scripts/install-asr.sh` in the same generation's source directory. Its displayed command is absolute and can be run from the project directory. Prepare local source checkouts and model files first, then supply their locations:

```bash
PILOT_HOME="<PilotDeck data directory>" \
PILOTDECK_FUNASR_SOURCE="<FunASR runtime-llamacpp-v0.2.0 checkout>" \
PILOTDECK_LLAMA_SOURCE="<pinned llama.cpp checkout>" \
PILOTDECK_FUNASR_MODELS_DIR="<local GGUF model directory>" \
sh "<installed generation>/source/scripts/install-asr.sh"
```

The source installer checks the pinned source revisions, builds the external ASR executable locally, and imports the already prepared model files into `$PILOT_HOME/funasr`. It does not download runtimes or models. Existing runtime/model files are retained. To use a separately managed installer, configure `PILOTDECK_FUNASR_INSTALL_COMMAND` before starting the Gateway; the diagnostic displays that exact command. After installation, retry `mcp__funasr__transcribe_audio` in the same session.

Separately packaged TypeScript desktop runtimes may still display their own bundled Node installation command; use the exact command shown by that runtime. The Rust source generation does not contain the old npm installation script.

## Workflow

1. Identify the registered audio attachment path in the session message.
2. Pass that project-local host path directly as `audio_path`; do not convert it to a container path. The tool rejects paths outside the current project, including symlinks that escape it.
3. Call `mcp__funasr__transcribe_audio` with `language: "auto"`. This is the only language value supported by the local SenseVoice CLI.
4. Preserve timestamped segments for subtitles, verbatim transcripts, or auditable meeting records.
5. Only after transcription, summarize, translate, or extract action items as requested.

## Constraints

- Do not use `read_file` to decode audio.
- Do not send the audio to a cloud service or substitute a different ASR provider without the user's direction.
- The tool is intentionally limited to a real local file inside the active project.

#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# Independently written integration wrapper; no upstream implementation is vendored.
# Build the pinned external ASR SDK locally; this script never downloads files.
# Build contract: modelscope/FunASR@500956bc331bb7edbaac58d8f84a84f28bd3d29f
# runtime/llama.cpp/CMakeLists.txt; dependency llama.cpp@803b7fcae893e9caaee3921779628fef83ac0965.
# Both external source repositories are MIT-licensed; retain their LICENSE files
# beside the built executable. This wrapper remains under repository AGPL-3.0-only.
# https://github.com/modelscope/FunASR/tree/500956bc331bb7edbaac58d8f84a84f28bd3d29f/runtime/llama.cpp
# https://github.com/ggml-org/llama.cpp/tree/803b7fcae893e9caaee3921779628fef83ac0965

set -eu
fail() { printf '%s\n' "[pilotdeck-asr] $*" >&2; exit 1; }
[ -n "${PILOT_HOME:-}" ] || fail 'Set PILOT_HOME to the intended writable PilotDeck data directory.'
[ -n "${PILOTDECK_FUNASR_SOURCE:-}" ] || fail 'Set PILOTDECK_FUNASR_SOURCE to the local FunASR runtime-llamacpp-v0.2.0 source checkout.'
[ -n "${PILOTDECK_LLAMA_SOURCE:-}" ] || fail 'Set PILOTDECK_LLAMA_SOURCE to the local pinned llama.cpp source checkout.'
[ -n "${PILOTDECK_FUNASR_MODELS_DIR:-}" ] || fail 'Set PILOTDECK_FUNASR_MODELS_DIR to the local directory containing sensevoice-small-q8.gguf and fsmn-vad.gguf.'
for tool in git cmake mktemp install find stat ps date head; do command -v "$tool" >/dev/null 2>&1 || fail "Required build tool is unavailable: $tool"; done
source_root=$(CDPATH= cd -- "$PILOTDECK_FUNASR_SOURCE" && pwd -P) || fail 'FunASR source directory is unavailable.'
llama_root=$(CDPATH= cd -- "$PILOTDECK_LLAMA_SOURCE" && pwd -P) || fail 'llama.cpp source directory is unavailable.'
models_root=$(CDPATH= cd -- "$PILOTDECK_FUNASR_MODELS_DIR" && pwd -P) || fail 'Model source directory is unavailable.'
[ "$(git -C "$source_root" rev-parse HEAD)" = 500956bc331bb7edbaac58d8f84a84f28bd3d29f ] || fail 'FunASR source does not match runtime-llamacpp-v0.2.0.'
[ "$(git -C "$llama_root" rev-parse HEAD)" = 803b7fcae893e9caaee3921779628fef83ac0965 ] || fail 'llama.cpp source does not match the pinned FunASR dependency.'
[ -z "$(git -C "$source_root" status --porcelain --untracked-files=all)" ] || fail 'FunASR source must be clean.'
[ -z "$(git -C "$llama_root" status --porcelain --untracked-files=all)" ] || fail 'llama.cpp source must be clean.'
[ -f "$source_root/runtime/llama.cpp/CMakeLists.txt" ] || fail 'FunASR runtime CMake entrypoint is missing.'
[ -s "$source_root/LICENSE" ] && [ -s "$llama_root/LICENSE" ] || fail 'Source license notices must be present.'
for model in sensevoice-small-q8.gguf fsmn-vad.gguf; do
    [ -f "$models_root/$model" ] && [ -s "$models_root/$model" ] || fail "Local model is missing or empty: $model"
done
case $(uname -s)/$(uname -m) in
    Darwin/arm64) platform=darwin-arm64 ;;
    Linux/aarch64|Linux/arm64) platform=linux-arm64 ;;
    Linux/x86_64) platform=linux-x64 ;;
    *) fail 'This source installer supports macOS ARM64 and Linux ARM64/x64.' ;;
esac
jobs=${PILOTDECK_ASR_BUILD_JOBS:-2}
case $jobs in ''|*[!0-9]*|0*) fail 'PILOTDECK_ASR_BUILD_JOBS must be a positive integer without leading zeroes.' ;; esac
# A broad hard ceiling rejects dangerous accidental fanout. The default is 2;
# operators still choose a value appropriate for the target machine.
if [ "${#jobs}" -gt 3 ] || [ "$jobs" -gt 128 ]; then
    fail "PILOTDECK_ASR_BUILD_JOBS must not exceed 128 (received $jobs)."
fi
umask 077
mkdir -p "$PILOT_HOME"
pilot_home=$(CDPATH= cd -- "$PILOT_HOME" && pwd -P)
cache=$pilot_home/funasr
for directory in "$cache" "$cache/runtime" "$cache/runtime/v0.2.0" "$cache/models"; do
    [ ! -L "$directory" ] || fail "Refusing a symlinked installer directory: $directory"
    [ ! -e "$directory" ] || [ -d "$directory" ] || fail "Installer directory is not a directory: $directory"
done
mkdir -p "$cache/runtime/v0.2.0" "$cache/models"
lock=$cache/.source-install.lock
if ! mkdir "$lock" 2>/dev/null; then
    printf '%s\n' "[pilotdeck-asr] Existing installation lease: $lock" >&2
    if [ ! -L "$lock" ] && [ -f "$lock/owner.txt" ] && [ ! -L "$lock/owner.txt" ]; then head -c 2048 "$lock/owner.txt" >&2; fi
    fail "Confirm the recorded installer and build processes have stopped before removing the recorded stage and this lock. No existing lock or stage was removed."
fi
stage=
lock_identity=
lock_marker=
lock_identity_now() {
    [ ! -L "$lock" ] && [ -d "$lock" ] || return 1
    case $platform in
        darwin-*) stat -f '%d:%i' "$lock" ;;
        *) stat -c '%d:%i' -- "$lock" ;;
    esac
}
cleanup() {
    if [ -n "$stage" ]; then rm -rf -- "$stage"; fi
    # A manually replaced lease belongs to another installer. Never remove it.
    [ -n "$lock_identity" ] || return 0
    current_identity=$(lock_identity_now 2>/dev/null) || return 0
    [ "$current_identity" = "$lock_identity" ] || return 0
    [ -n "$lock_marker" ] && [ -f "$lock_marker" ] && [ ! -L "$lock_marker" ] || return 0
    rm -f -- "$lock/owner.txt" "$lock_marker"
    current_identity=$(lock_identity_now 2>/dev/null) || return 0
    [ "$current_identity" = "$lock_identity" ] || return 0
    rmdir "$lock" 2>/dev/null || :
}
trap cleanup EXIT
# A foreground build may still own outputs when the shell receives a signal.
# Keep its private stage and lock rather than deleting an in-flight build tree.
interrupted() {
    trap - EXIT
    printf '%s\n' "[pilotdeck-asr] Interrupted; preserve $lock and $stage until the build is confirmed stopped." >&2
    exit 143
}
trap interrupted INT TERM HUP
lock_identity=$(lock_identity_now)
lock_marker=$(mktemp "$lock/.owner.XXXXXXXX")
printf 'installerPid=%s\nstartedUtc=%s\nbirth=%s\n' "$$" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$(ps -p "$$" -o lstart=)" > "$lock/owner.txt"
stage=$(mktemp -d "$cache/.source-build.XXXXXXXX")
printf 'stage=%s\n' "$stage" >> "$lock/owner.txt"
device() {
    case $platform in darwin-*) stat -f '%d' "$1" ;; *) stat -c '%d' -- "$1" ;; esac
}
stage_device=$(device "$stage")
[ "$(device "$cache/runtime/v0.2.0")" = "$stage_device" ] && [ "$(device "$cache/models")" = "$stage_device" ] || fail 'Installer stage and destination must share a filesystem for atomic publication.'
target=$cache/runtime/v0.2.0/$platform
[ ! -L "$target" ] || fail 'Refusing a symlinked runtime directory.'
# Stage every new model before compilation/publication; a failed build cannot
# replace an installed runtime or leave a partial model at its final pathname.
for model in sensevoice-small-q8.gguf fsmn-vad.gguf; do
    destination=$cache/models/$model
    [ ! -L "$destination" ] || fail "Refusing a symlinked model: $destination"
    if [ -e "$destination" ]; then
        [ -f "$destination" ] && [ -s "$destination" ] || fail "Existing model is incomplete: $destination"
    else
        install -m 600 "$models_root/$model" "$stage/$model"
    fi
done
if [ -e "$target" ]; then
    installed=$(find "$target" -type f -name llama-funasr-sensevoice -print -quit)
    [ -n "$installed" ] && [ -x "$installed" ] || fail 'Existing runtime directory is incomplete; preserve it and inspect it before retrying.'
    printf '%s\n' "[pilotdeck-asr] Reusing installed runtime: $target"
else
    printf '%s\n' '[pilotdeck-asr] Building pinned FunASR and local llama.cpp sources; downloads are disabled.'
    cmake -S "$source_root/runtime/llama.cpp" -B "$stage/build" \
        -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DFETCHCONTENT_SOURCE_DIR_LLAMA="$llama_root" \
        -DFETCHCONTENT_FULLY_DISCONNECTED=ON -DFETCHCONTENT_UPDATES_DISCONNECTED=ON \
        -DGGML_NATIVE=OFF -DGGML_CUDA=OFF -DGGML_VULKAN=OFF
    cmake --build "$stage/build" --config Release --target llama-funasr-sensevoice --parallel "$jobs"
    built=$stage/build/bin/llama-funasr-sensevoice
    [ -f "$built" ] && [ -x "$built" ] || fail 'Source build did not produce an executable llama-funasr-sensevoice.'
    mkdir "$stage/runtime"
    install -m 700 "$built" "$stage/runtime/llama-funasr-sensevoice"
    install -m 600 "$source_root/LICENSE" "$stage/runtime/FunASR-LICENSE"
    install -m 600 "$llama_root/LICENSE" "$stage/runtime/llama.cpp-LICENSE"
    printf '%s\n' 'FunASR=500956bc331bb7edbaac58d8f84a84f28bd3d29f' \
        'llama.cpp=803b7fcae893e9caaee3921779628fef83ac0965' > "$stage/runtime/source-revisions.txt"
    mv "$stage/runtime" "$target"
fi
for model in sensevoice-small-q8.gguf fsmn-vad.gguf; do
    destination=$cache/models/$model
    if [ -e "$destination" ]; then
        [ -f "$destination" ] && [ -s "$destination" ] || fail "Existing model is incomplete: $destination"
        printf '%s\n' "[pilotdeck-asr] Reusing local model: $destination"
    else
        mv "$stage/$model" "$destination"
    fi
done
printf '%s\n' "[pilotdeck-asr] Local runtime and models are prepared at $cache. Retry mcp__funasr__transcribe_audio."
printf '%s\n' '[pilotdeck-asr] Actual transcription still requires a real audio/model validation; file presence alone does not verify model contents.'

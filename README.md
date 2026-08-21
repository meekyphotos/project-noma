# Noma

Local push-to-talk dictation. Hold a hotkey, speak, release, and text is pasted into the focused app.

This is the Windows skeleton: the PTT loop is real, transcription is still a placeholder. Next up is NVIDIA Parakeet on the GPU.

## Run (Windows)

```bash
cargo run -p noma
```

1. A teal tray icon appears.
2. Click **Preview HUD** in the tray to see the overlay.
3. Click a text field, hold **Right Ctrl**, speak, then release.
4. Noma pastes `Hello from Noma (N.Ns)` — `N.N` is how long you held the key.
5. **Quit** from the tray.

Right Ctrl is swallowed while Noma is running so it does not act as a modifier in other apps.

## Architecture

| Crate | Role |
|---|---|
| `noma` | Binary: wires the loop |
| `noma-hotkey` | Global hold-to-talk (Right Ctrl) |
| `noma-audio` | Mic capture + waveform peaks |
| `noma-asr` | `AsrEngine` trait + `FakeEngine` |
| `noma-inject` | Clipboard + Ctrl+V into the focused app |
| `noma-hud` | Transparent waveform overlay + tray |

ASR is a trait so Parakeet (ONNX / CUDA) can replace `FakeEngine` without rewriting the app. Whisper.cpp is a later optional backend for languages Parakeet does not cover.

## Next

- Parakeet TDT 0.6B v3 on the RTX 4080 SUPER
- Streaming partials in the HUD
- macOS (Accessibility + CoreML/Metal)
- Settings, installer, dictation commands

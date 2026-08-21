# Noma

Local push-to-talk dictation. Hold a hotkey, speak, release, and text is pasted into the focused app. Nothing leaves the machine.

Transcription is NVIDIA Parakeet TDT 0.6B, running locally through [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx).

## Run (Windows)

```bash
cargo run -p noma
```

On the **first** run Noma downloads the model (~465 MB) into `%LOCALAPPDATA%\noma\models`. The HUD shows a progress bar while it does; the tray and hotkey work throughout. Every run after that starts offline in a second or two.

1. A teal tray icon appears.
2. Wait for the HUD to stop saying **Setting up**.
3. Click a text field, hold **Right Ctrl**, speak, then release.
4. The words appear where the cursor is.
5. **Quit** from the tray.

Right Ctrl is swallowed while Noma runs, so it does not act as a modifier in other apps.

Quit any previous `noma.exe` before `cargo run` or the build cannot replace the binary.

To try the UI without the download:

```bash
cargo run -p noma -- --fake
```

To hold the HUD on screen and look at it, without dictating or downloading:

```bash
cargo run -p noma -- --preview
```

## Tray

| Item | What it does |
|---|---|
| Preview HUD | Shows the pill for three seconds |
| History... | Recent dictations, with copy buttons |
| Copy last transcript | Puts the last dictation back on the clipboard |
| Open settings folder | Opens `%APPDATA%\noma` |

## Settings

`%APPDATA%\noma\settings.toml`, written on first run and read at startup. A file that fails to parse is moved to `settings.toml.invalid` rather than overwritten.

```toml
model = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"
provider = "cpu"          # cpu | cuda | directml
threads = 8
hotkey = "right-ctrl"     # right-ctrl | right-alt | right-shift | caps-lock | f13
microphone = ""           # substring of a device name; empty = Windows default
paste-mode = "paste"      # paste (Ctrl+V) | type (synthesized keystrokes)
hud-opacity = 0.6         # 0.0 fully see-through, 1.0 solid
partials = true           # decode while the key is held
partial-interval-ms = 700
history-limit = 200       # 0 turns history off entirely

[text]
enabled = true
spoken-commands = true
remove-fillers = true
fillers = ["um", "uh", "uhm", "erm", "eh", "mm", "hmm", "mhm"]
capitalize-sentences = true
ensure-final-punctuation = false

[[text.replacements]]
from = "no ma"
to = "Noma"
```

### Models

| Id | What it is |
|---|---|
| `sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8` | 25 European languages, punctuated (default) |
| `sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8` | English only, a little sharper on English |

Both punctuate and capitalize on their own, which is why the text stage below does not try to.

### Microphone

Empty means Noma follows the Windows default input, resolved fresh on every key
press, so changing the default takes effect on the next dictation without a
restart.

That default is not always the mic you are speaking into. A wireless headset
that is off or on its charger still registers as a perfectly healthy device and
records digital silence, which looks exactly like Noma ignoring you. Set
`microphone` to pin a specific one instead:

```toml
microphone = "yeti"       # matches "Microphone (Yeti Classic)"
```

Matching is a case-insensitive substring, with an exact name winning over a
partial one, so a short name is enough and it survives Windows renumbering a
device to `2- Arctis Nova Pro Wireless`. An unmatched name falls back to the
default rather than refusing to record, and says so on stderr.

To see what is available and which one Noma would use:

```bash
cargo run -p noma-audio --example devices
```

To find out which mic can actually hear you - a live mic has a noise floor even
in a silent room, a muted or sleeping one returns digital silence:

```bash
cargo run -p noma-audio --example levels
```

### Spoken commands

Said out loud, these become characters: `new line`, `new paragraph`, `question mark`, `exclamation mark`, `semicolon`, `ellipsis`. Deliberately absent are "period" and "comma" — Parakeet punctuates on its own, and dictating a sentence containing the word "period" is far likelier than wanting a bare full stop.

A phrase only counts when the model decoded it bare, so "a new. line" stays literal.

## The HUD is genuinely transparent

Windows will not do this for you. Setting `DWMWA_SYSTEMBACKDROP_TYPE` to acrylic
looks like the right call and is the opposite: DWM paints that material across
the whole window rect *underneath* the app's own pixels, so every transparent
pixel shows material instead of the desktop. Worse, the HUD is deliberately
never activated (it must not steal focus from what you are dictating into), and
Windows degrades acrylic on a deactivated window to a flat solid colour. The
result was an opaque grey slab that looked like a bug and was in fact the
documented behaviour.

`with_transparent(true)` and a system backdrop are mutually exclusive
intentions: winit implements transparency by marking the client area
DWM-transparent, which is precisely the condition that lets a backdrop paint
beneath it. Noma therefore asks for `DWMSBT_NONE` and paints its own
translucency, tuned with `hud-opacity`.

That opacity is the only thing between the subtitle and your wallpaper, so it
trades see-through against readability. Below about 0.35 the secondary text
starts to disappear over a busy background.

## Partial results

With `partials = true`, Noma re-decodes the whole buffer every ~700 ms while the key is held and shows the text in the HUD. Parakeet has no streaming mode, so this is repeated offline decoding rather than true streaming: the interval adapts to how long a pass takes, one decode runs at a time, and passes stop after 30 seconds of held audio so they never delay the final result. Set `partials = false` for the lowest possible latency on release.

## GPU

The default build downloads a prebuilt CPU sherpa-onnx, and that is fast enough that the GPU is optional: on a Ryzen-class CPU with 4 threads, Parakeet int8 decodes a 3.85 s clip in 0.14 s — about **27x faster than real time**. A ten-second dictation comes back in well under half a second.

`provider = "cuda"` or `"directml"` additionally needs sherpa-onnx built with that backend:

```bash
cargo build -p noma --features cuda
```

That compiles sherpa-onnx from source (CMake, MSVC, and a CUDA toolkit matching the ONNX Runtime build) instead of downloading it, and takes considerably longer. Setting `provider` without the matching feature falls back to CPU inside ONNX Runtime.

## Architecture

| Crate | Role |
|---|---|
| `noma` | Binary: wires the loop, loads the model in the background |
| `noma-hotkey` | Global hold-to-talk, one of five bindable keys |
| `noma-audio` | Mic capture, waveform peaks, band-limited resampling to 16 kHz |
| `noma-asr` | `AsrEngine` trait, Parakeet engine, and a slot for "not loaded yet" |
| `noma-model` | Model registry, download with progress, unpack |
| `noma-text` | Fillers, spoken commands, custom vocabulary, capitalization |
| `noma-config` | `settings.toml` and the dictation history |
| `noma-inject` | Clipboard + Ctrl+V, or synthesized typing |
| `noma-hud` | Transparent waveform overlay, mascot, tray, history window |

ASR is behind a trait, so Whisper.cpp can be added later for languages Parakeet does not cover without touching the app.

## Tests

```bash
cargo test --workspace
```

Two suites are ignored by default because they need hardware or the download. End to end against the real model, in English, German, French and Spanish:

```bash
cargo test -p noma-asr --test parakeet -- --ignored --nocapture
```

Microphone capture, snapshotting and resampling against the real input device:

```bash
cargo test -p noma-audio --test capture -- --ignored --nocapture
```

Two examples help when dictation records nothing: `devices` lists the mics and
marks the one Noma would use, and `levels` measures what each one is hearing.

## Next

- Streaming partials that decode incrementally instead of re-decoding
- Optional LLM cleanup pass for stutters and restarts
- macOS (Accessibility + CoreML/Metal)
- Settings UI, installer, dictation commands

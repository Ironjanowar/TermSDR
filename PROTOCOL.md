# TermSDR WebSocket Protocol

> Protocol reference for third-party clients connecting to TermSDR's headless WebSocket server.

**Version:** 1.0  
**Transport:** WebSocket (RFC 6455)

---

## Connection

```
ws://<host>:<port>/ws
```

Default: `ws://localhost:8080/ws`

The HTTP server also serves the built-in web frontend at `GET /` (unless `--no-web` is used).

### Connection Flow

1. Client opens WebSocket connection to `/ws`
2. Server immediately sends a **`state`** JSON message with full current state
3. Server begins streaming **binary frames** (spectrum + audio) to all connected clients
4. Client may send **JSON commands** at any time to control the SDR
5. On state-changing commands, server broadcasts updated `state` to **all** clients
6. On error, server sends an `error` message to the **requesting client only**

### Multi-Client Behavior

- Multiple clients can connect simultaneously
- All clients receive the same spectrum and audio streams
- Any client can send commands (shared control, no roles/auth)
- State changes are broadcast to all connected clients
- The `clients_connected` field in state messages reflects the current count

---

## Binary Frames (Server → Client)

Binary frames carry high-frequency data (spectrum, audio). The first byte is a **tag** identifying the frame type.

### Spectrum Frame (`0x01`)

Sent at ~30 fps. Contains FFT magnitude data in dB.

```
Offset  Size     Type        Description
──────  ─────    ─────       ───────────
0       1        u8          Tag: 0x01
1       8        u64 LE      Timestamp (ms since Unix epoch)
9       2        u16 LE      Number of bins (N)
11      N × 4    f32 LE[]    dB magnitude values (one per bin)
```

**Total size:** `11 + N × 4` bytes (e.g., 8,203 bytes for 2048 bins)

Bins are ordered from lowest to highest frequency across the current bandwidth:
- Bin 0 → `center_freq - sample_rate / 2`
- Bin N-1 → `center_freq + sample_rate / 2`

### Audio Frame (`0x02`)

Streamed continuously (~50 frames/s for Opus, variable for PCM).

```
Offset  Size     Type        Description
──────  ─────    ─────       ───────────
0       1        u8          Tag: 0x02
1       1        u8          Audio format: 0x00 = Opus, 0x01 = PCM f32 raw
2       varies   bytes       Audio payload
```

**Opus format (`0x00`):** Each frame is a complete Opus packet. Decode with any Opus decoder. Sample rate: 48 kHz, mono.

**PCM raw format (`0x01`):** Interleaved f32 little-endian samples. Sample rate: 48 kHz, mono.

---

## JSON Text Frames

### Server → Client

#### `state`

Sent on connect and after every state-changing command. Contains the complete SDR state.

```json
{
  "type": "state",
  "center_freq": 118100000,
  "sample_rate": 2048000,
  "fft_size": 2048,
  "gain": 30,
  "gain_mode": "manual",
  "cursor_freq": 118100000,
  "cursor_active": true,
  "demod_mode": "AM",
  "channel_bw": 8000,
  "volume": 80,
  "squelch": -60,
  "muted": false,
  "agc": {
    "enabled": true,
    "attack_ms": 10,
    "decay_ms": 300,
    "target_level": 0.3,
    "max_gain_db": 50
  },
  "bandpass": {
    "enabled": true,
    "low_hz": 300,
    "high_hz": 3400
  },
  "vfos": [
    {
      "id": 1,
      "freq": 118100000,
      "channel_bw": 8000,
      "mode": "AM",
      "color": "red"
    }
  ],
  "audio_format": "opus",
  "clients_connected": 2
}
```

| Field | Type | Description |
|-------|------|-------------|
| `center_freq` | `f64` | Center frequency in Hz |
| `sample_rate` | `f64` | Sample rate / bandwidth in Hz |
| `fft_size` | `usize` | Number of FFT bins |
| `gain` | `f32 \| null` | RF gain in dB (`null` = AGC) |
| `gain_mode` | `string` | `"manual"` or `"auto"` |
| `cursor_freq` | `f64` | Current cursor/tuning frequency in Hz |
| `cursor_active` | `bool` | Whether cursor is active (demodulating) |
| `demod_mode` | `string` | `"AM"`, `"FM"`, or `"NFM"` |
| `channel_bw` | `f64` | Channel bandwidth in Hz |
| `volume` | `u8` | Volume 0–100 |
| `squelch` | `f32` | Squelch threshold in dB |
| `muted` | `bool` | Whether audio is muted |
| `agc` | `object` | AGC settings (see above) |
| `bandpass` | `object` | Bandpass filter settings (see above) |
| `vfos` | `array` | Active VFOs |
| `audio_format` | `string` | `"opus"` or `"pcm"` |
| `clients_connected` | `usize` | Number of connected WebSocket clients |

#### `error`

Sent only to the client that issued an invalid command.

```json
{
  "type": "error",
  "cmd": "set_frequency",
  "message": "Invalid frequency: must be between 24MHz and 1.766GHz"
}
```

---

### Client → Server (Commands)

All commands are JSON text frames with a `"cmd"` field.

#### Navigation

```json
{"cmd": "set_frequency", "value": 118100000}
```
Set the cursor/tuning frequency in Hz.

```json
{"cmd": "tune", "delta": 25000}
```
Shift cursor frequency by `delta` Hz (positive = up, negative = down).

```json
{"cmd": "set_bandwidth", "value": 2048000}
```
Set the sample rate / visible bandwidth in Hz.

```json
{"cmd": "zoom", "direction": "in"}
```
Zoom in or out. `direction`: `"in"` or `"out"`.

#### Audio

```json
{"cmd": "set_volume", "value": 80}
```
Set volume (0–100).

```json
{"cmd": "set_squelch", "value": -60}
```
Set squelch threshold in dB.

```json
{"cmd": "set_demod", "mode": "AM"}
```
Set demodulation mode. Values: `"AM"`, `"FM"`, `"NFM"`.

```json
{"cmd": "set_channel_bw", "value": 8000}
```
Set channel bandwidth in Hz.

```json
{"cmd": "toggle_mute"}
```
Toggle audio mute.

#### VFOs

```json
{"cmd": "vfo_add", "freq": 118100000, "channel_bw": 8000, "mode": "AM"}
```
Add a VFO at the given frequency. Max 8 VFOs.

```json
{"cmd": "vfo_remove", "id": 3}
```
Remove VFO by ID.

```json
{"cmd": "vfo_clear"}
```
Remove all VFOs.

#### AGC

```json
{"cmd": "set_agc", "enabled": true, "attack_ms": 10, "decay_ms": 300, "target_level": 0.3, "max_gain_db": 50}
```
Configure AGC. Only `enabled` is required; other fields default to: `attack_ms=10`, `decay_ms=300`, `target_level=0.3`, `max_gain_db=50`.

#### Bandpass Filter

```json
{"cmd": "set_bandpass", "enabled": true, "low_hz": 300, "high_hz": 3400}
```
Configure bandpass filter. Only `enabled` is required; defaults: `low_hz=300`, `high_hz=3400`.

#### System

```json
{"cmd": "set_gain", "value": 30}
```
Set RF gain in dB.

```json
{"cmd": "get_state"}
```
Request current state. Server responds with a `state` message to the requesting client only.

```json
{"cmd": "set_audio_format", "format": "opus"}
```
Switch audio format. Values: `"opus"`, `"pcm"`.

---

## Audio Format Negotiation

The server starts with the audio format specified by the `--audio-format` CLI flag (default: `opus`).

Any connected client can change the format at runtime via `set_audio_format`. This affects **all** clients (shared state). The change takes effect immediately — subsequent audio binary frames will use the new format byte.

Clients should check the format byte (offset 1) of each audio binary frame to determine how to decode it, rather than relying solely on the `audio_format` field in state messages.

---

## Example Session

```
Client                              Server
  │                                    │
  ├──── WS connect /ws ───────────────►│
  │                                    │
  │◄───── {"type":"state", ...} ───────┤  (full state on connect)
  │                                    │
  │◄───── [0x01][spectrum data] ───────┤  (30 fps)
  │◄───── [0x02][audio data] ──────────┤  (streaming)
  │◄───── [0x01][spectrum data] ───────┤
  │                                    │
  ├── {"cmd":"set_frequency",  ───────►│
  │    "value":121500000}              │
  │                                    │
  │◄───── {"type":"state", ...} ───────┤  (broadcast to all clients)
  │                                    │
  ├── {"cmd":"set_demod", ────────────►│
  │    "mode":"FM"}                    │
  │                                    │
  │◄───── {"type":"state", ...} ───────┤  (broadcast)
  │                                    │
  ├── {"cmd":"invalid_cmd"} ──────────►│
  │                                    │
  │◄───── {"type":"error", ...} ───────┤  (to this client only)
  │                                    │
```

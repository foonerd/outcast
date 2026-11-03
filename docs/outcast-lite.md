# outcast-lite

The player's sound, as it is, over a WebSocket: for a page or a remote that wants to hear what the player plays where the person is. One small static binary per architecture, no encoder, no configuration file. It reads the audio ring that [evotap](https://github.com/foonerd/evotap) writes from the player's audio chain and serves the PCM to any WebSocket client, one text frame to say what the stream is, then binary frames of at most 20 ms each. It is built and released with Outcast Uplink and shares its code, but it is not the plugin and needs none of it.

## Getting it

Every [Outcast release](https://github.com/foonerd/outcast/releases) from 0.1.5 carries, beside the plugin zip, one archive per Volumio architecture:

```
outcast-lite-<version>-arm.tar.gz      ARMv7 (Volumio's universal Raspberry Pi image calls itself arm)
outcast-lite-<version>-armv7.tar.gz
outcast-lite-<version>-armv8.tar.gz
outcast-lite-<version>-x64.tar.gz
```

Each unpacks to `outcast-lite-<version>-<arch>/` with `bin/outcast-lite`, this document as `README.md`, and `LICENSE`, and carries `MANIFEST` (every file's SHA-256) with `MANIFEST.sig`, its Ed25519 signature, at the archive's root. Beside the assets, `SHA256SUMS` and `SHA256SUMS.sig`. The key is the evo release key, the one Glass and Outcast carry as `keys/release-signing.pub`:

```sh
openssl pkeyutl -verify -pubin -inkey release-signing.pub -rawin -in SHA256SUMS -sigfile SHA256SUMS.sig
grep " outcast-lite-0.1.5-armv8.tar.gz" SHA256SUMS | sha256sum -c -
tar -xzf outcast-lite-0.1.5-armv8.tar.gz
openssl pkeyutl -verify -pubin -inkey release-signing.pub -rawin -in MANIFEST -sigfile MANIFEST.sig
sha256sum -c MANIFEST
```

The binary is linked statically against musl: it needs nothing from the player's C library and runs on Volumio 3 and 4 alike. A lock file in the consumer's repository naming the version, and a fetch that does the five lines above in CI, is how Outcast itself takes evotap and evopeg; `scripts/fetch-components.sh` in the Outcast repository is that fetch and can be copied.

## What it needs on the player

The tap in the player's audio chain, writing the audio ring for the tag the lite reads. evotap writes the audio ring only when its ALSA snippet says so:

```
pcm.Glass {
  type evotap
  slave.pcm "postglass"
  ring "glass"
  audio 1
}
```

Without `audio 1` the tap measures and writes no audio, and the lite finds no ring: it listens, says so, and serves nothing until one appears. The ring costs the player one copy of the stream's bytes into shared memory per period; two seconds of the stream are held (`audio_seconds`, 1 to 10). The ring is `/dev/shm/evoaudio.<tag>.<pid>.<n>`; the lite takes the newest live one of its tag, so a tap that is restarted is followed.

## Running it

```
outcast-lite [--tag TAG] [--shm DIR] [--bind ADDR] [--port N] [--max N] [--idle SECONDS] [--cap RATE] [--version]
```

| Option | Default | Meaning |
| --- | --- | --- |
| `--tag` | `glass` | The ring's tag, the `ring` word of the snippet. |
| `--shm` | `/dev/shm` | Where the rings are. |
| `--bind` | `127.0.0.1` | The address to listen on. The lite speaks plain `ws://`; put it behind the manager, which the page already talks to, rather than on the network. |
| `--port` | `0` | The port; `0` lets the system choose and the lite says which on standard output. |
| `--max` | `8` | Listeners at once; the next is told `full` and closed. |
| `--idle` | `30` | Seconds without a listener after which the lite exits 0; `0` never. Counted from the start, so a lite started and never asked leaves too. |
| `--cap` | `0` | A sample rate above which the stream is resampled down to it and sent as 16-bit; `0` sends every stream as it is. |

The intended life is on demand: the manager starts it when the first page asks for the sound, reads the `listening` line for the port, proxies the page's WebSocket to it, and lets it leave by itself when nobody has listened for `--idle` seconds. Starting it with the player and `--idle 0` works too. `SIGTERM` ends it at once; it holds nothing that needs a clean close.

Standard output carries one JSON line per event, nothing else:

```json
{"type":"listening","address":"127.0.0.1:41234","tag":"glass"}
{"type":"segment","rate":44100,"channels":2,"format":"s16le","bits":16,"frame_bytes":4,"one_bit":false,"capped":false,"chunk_ms":20}
{"type":"listeners","count":1}
{"type":"end"}
{"type":"listeners","count":0}
{"type":"exit","reason":"idle","seconds":30}
```

Standard error carries its log lines, prefixed `outcast-lite:`.

## The wire

A client opens a WebSocket (`ws://127.0.0.1:<port>/`, any path) and sends nothing; pings are answered. The server sends text frames, each one JSON object with a `type`, and binary frames, each a piece of the stream.

| Text frame | When | Fields |
| --- | --- | --- |
| `hello` | First, on connect | `version`, the lite's; `tag`. |
| `segment` | A stream begins, and on connect while one is on | `rate` Hz; `channels`; `format`: `s16le`, `s24le`, `s32le` or `f32le`, interleaved, little-endian; `bits` 16, 24 or 32; `frame_bytes`, one frame of every channel; `one_bit`; `capped`; `chunk_ms`. |
| `end` | The stream ended (the player stopped, or changed its format: a new `segment` follows) | |
| `gap` | The client fell behind and frames were dropped for it | `dropped`, how many. Reset the buffer and go on. |
| `full` | Too many listeners; the connection closes | |

Binary frames carry whole PCM frames of the segment's shape, at most `chunk_ms` of audio each, in order, as the player writes them, so they arrive at the player's own pace. Nothing is sent while the player stands still: a page that wants silence then makes its own. Between `end` and the next `segment` nothing is sent either.

A segment with `one_bit` true is one-bit audio (DSD, as DoP or native) that a page cannot play: it is announced and no frames follow until the next segment. Say so on the page.

`s24le` is three bytes a sample; `s16le`, `s32le` and `f32le` are what their names say. Samples are at the player's level before its software volume where the tap sits before it, which is the usual place: the page should offer its own gain.

Frames a client has not taken are held for it up to two seconds, then dropped and counted; a client that stops taking bytes for three seconds is closed. The lite never waits on a client; the ring is read whatever the clients do.

## The rate cap

With `--cap 48000`, a stream above 48 kHz is resampled in the lite with a windowed sinc (`rubato`, the resampler the Outcast core uses) and sent as `s16le` at the cap, `capped` true; a stream at or below the cap passes untouched. For a phone on weak Wi-Fi or a television, 44.1 or 48 kHz at 16-bit is 1.4 to 1.5 Mbit/s stereo; a 192 kHz 24-bit stream untouched is 9.2 Mbit/s. The cap is for the manager to set from a page setting; the stream the player hears is never touched.

## Playing it in a page

Web Audio takes PCM straight: decode the bytes of each binary frame to `Float32Array`s per channel, put them into an `AudioBuffer` at the segment's rate and schedule it on the context's clock right after the previous one, with a jitter buffer of 150 to 500 ms ahead of the clock. The browser resamples to the device's rate. `AudioBufferSourceNode` scheduling is in every browser that has Web Audio; an `AudioWorklet` with a ring buffer is steadier under load where it exists (Chromium 66, Firefox 76, Safari 14.1 and later) and takes the same frames.

A sketch of the simple path, enough to hear it:

```js
const ctx = new AudioContext();            // resumed inside a tap on the page: iOS wants a gesture
const gain = ctx.createGain(); gain.connect(ctx.destination);
let seg = null, at = 0;
const ws = new WebSocket('/anymote/listen'); ws.binaryType = 'arraybuffer';
ws.onmessage = (e) => {
  if (typeof e.data === 'string') {
    const m = JSON.parse(e.data);
    if (m.type === 'segment') { seg = m; at = 0; }
    if (m.type === 'end' || m.type === 'gap') { at = 0; }
    return;
  }
  if (!seg || seg.one_bit) return;
  const frames = e.data.byteLength / seg.frame_bytes;
  const buf = ctx.createBuffer(seg.channels, frames, seg.rate);
  const v = new DataView(e.data);
  for (let c = 0; c < seg.channels; c++) {
    const out = buf.getChannelData(c);
    for (let i = 0; i < frames; i++) {
      const p = i * seg.frame_bytes + c * (seg.bits / 8);
      out[i] = seg.format === 's16le' ? v.getInt16(p, true) / 32768
             : seg.format === 's24le' ? ((v.getUint8(p) | (v.getUint8(p + 1) << 8) | (v.getInt8(p + 2) << 16))) / 8388608
             : seg.format === 's32le' ? v.getInt32(p, true) / 2147483648
             : v.getFloat32(p, true);
    }
  }
  const src = ctx.createBufferSource(); src.buffer = buf; src.connect(gain);
  const lead = 0.25;                       // the jitter buffer: a quarter second ahead of the clock
  if (at < ctx.currentTime + 0.02) at = ctx.currentTime + lead;
  src.start(at); at += buf.duration;
};
```

The page hears the player a little behind the room: the ring, the frames and the jitter buffer together, a quarter to half a second with the lead above. Say so where the page offers the sound.

## Behind the manager

The page talks to the manager already; the lite listens on the loopback. Node proxies a WebSocket upgrade socket to socket with no library:

```js
server.on('upgrade', (req, socket, head) => {
  if (req.url !== '/anymote/listen') return socket.destroy();
  const up = net.connect(litePort, '127.0.0.1', () => {
    up.write(`GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n` +
             `Sec-WebSocket-Key: ${req.headers['sec-websocket-key']}\r\nSec-WebSocket-Version: 13\r\n\r\n`);
    if (head.length) up.write(head);
    up.pipe(socket); socket.pipe(up);
  });
  up.on('error', () => socket.destroy()); socket.on('error', () => up.destroy());
});
```

The lite's answer to the upgrade goes back through the same socket, so the page sees one origin, one port and the manager's own access control.

## Versions

The lite's version is Outcast's, said in `hello`. The wire is a contract like the ring's: a field is never renamed or removed within a major version; additions come as new fields and new `type`s a client may ignore. The `segment` fields above are the first version's whole set.

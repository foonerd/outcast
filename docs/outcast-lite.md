# Outcast Lite

The player's sound, as it is, over a WebSocket. Outcast Lite is one small static binary that reads the audio the [evotap](https://github.com/foonerd/evotap) tap takes from a Volumio player's audio chain and serves it as PCM to any WebSocket client: no encoder, no transcoding, no configuration file, nothing of the player touched. It exists so that a page or a remote display can play what the player plays where the person is, with Glass's Anymote and its remotes as the first takers. It is built from the same code as [Outcast Uplink](https://github.com/foonerd/outcast) and shares its tap, but it is not the plugin and needs none of it.

## What it is, and what it is not

- **It is** a component for other software on the player: started when something asks for the sound, gone when nothing has listened for a while, speaking on the loopback to whatever started it. The Glass manager starts it, proxies the page's WebSocket to it and stops nothing; the lite leaves by itself.
- **It carries the stream whole.** The tap gives the bytes the player sends to its DAC, before the player's software volume, at the stream's own rate and width; the lite hands them on unchanged, 16-bit as 16-bit, 24-bit as 24-bit, 192 kHz as 192 kHz. A rate cap, set by whoever starts it, brings a hi-res stream down for a weak link; nothing else is ever touched.
- **It is not** an uplink, a server for the network, or a transcoder. Outcast Uplink does those. The lite serves listeners on the player's own loopback and leaves the network, the access control and the page to the software in front of it.
- **It does not play DSD.** One-bit audio is announced as such and not sent; a page says "plays on the player only".

## How it works

```text
player ──ALSA──▶ evotap ──ring in /dev/shm──▶ outcast-lite ──WebSocket──▶ manager proxy ──▶ page
                 (audio 1)                     text: what the stream is
                                               binary: 20 ms of PCM at a time
```

evotap writes the audio ring, two seconds of the stream in shared memory, when its ALSA snippet says `audio 1`. The lite follows the newest live ring of its tag, as the Outcast core does: a new ring is a new segment, announced to every client; the ring going is the segment's end. Each client is a thread with a bounded queue, fed by the reader without waiting; a client that lags loses frames and is told; one that stops taking bytes is dropped. The reader never waits on a client, so a slow phone costs the player nothing.

The page hears the player a little behind the room: the ring, the 20 ms frames and the page's own jitter buffer together, a quarter to half a second with the buffer the reference below uses. A page that offers the sound should say so.

## Getting it

Every [Outcast release](https://github.com/foonerd/outcast/releases) from 0.1.5 carries, beside the plugin zip, one archive per Volumio architecture:

| Archive | For |
| --- | --- |
| `outcast-lite-<version>-arm.tar.gz` | Volumio's universal Raspberry Pi image, which calls itself `arm`; ARMv7 code, so not a Pi Zero or Pi 1 |
| `outcast-lite-<version>-armv7.tar.gz` | 32-bit ARM players |
| `outcast-lite-<version>-armv8.tar.gz` | 64-bit ARM players |
| `outcast-lite-<version>-x64.tar.gz` | x86-64 players |

Each unpacks to `outcast-lite-<version>-<arch>/` with `bin/outcast-lite`, this document as `README.md`, and `LICENSE`, and carries at its root `MANIFEST`, every file's SHA-256, with `MANIFEST.sig`, its Ed25519 signature. Beside the assets sit `SHA256SUMS` and `SHA256SUMS.sig`. The key is the evo release key, the one Glass and Outcast carry as `keys/release-signing.pub`:

```sh
openssl pkeyutl -verify -pubin -inkey release-signing.pub -rawin -in SHA256SUMS -sigfile SHA256SUMS.sig
grep " outcast-lite-0.1.5-armv8.tar.gz" SHA256SUMS | sha256sum -c -
tar -xzf outcast-lite-0.1.5-armv8.tar.gz
openssl pkeyutl -verify -pubin -inkey release-signing.pub -rawin -in MANIFEST -sigfile MANIFEST.sig
sha256sum -c MANIFEST
```

The binary is linked statically against musl and runs on Volumio 3 and 4 alike; it needs nothing from the player's C library. The way Outcast takes evotap and evopeg is the way to take the lite: a lock file naming the version, and a fetch in CI that does the five lines above and packs `bin/outcast-lite` for each architecture. `scripts/fetch-components.sh` in the Outcast repository is that fetch, ready to copy.

## What it needs on the player

The tap in the player's audio chain, with the audio ring on for the tag the lite reads. evotap measures by default and writes the audio ring only when told:

```text
pcm.Glass {
  type evotap
  slave.pcm "postglass"
  ring "glass"
  audio 1
}
```

Without `audio 1` there is no ring: the lite listens, says so, and serves nothing until one appears. The ring costs the player one copy of the stream's bytes per period into shared memory, and holds two seconds of it (`audio_seconds`, 1 to 10). The rings are `/dev/shm/evoaudio.<tag>.<pid>.<n>`; the lite takes the newest live one of its tag, so a tap restarted is followed.

The lite itself costs nothing to speak of: no encoder runs, and the bytes are copied once per client.

## Running it

```text
outcast-lite [--tag TAG] [--shm DIR] [--bind ADDR] [--port N] [--max N] [--idle SECONDS] [--cap RATE] [--version]
```

| Option | Default | Meaning |
| --- | --- | --- |
| `--tag` | `glass` | The ring's tag, the `ring` word of the snippet. |
| `--shm` | `/dev/shm` | Where the rings are. |
| `--bind` | `127.0.0.1` | The address to listen on. The lite speaks plain `ws://`; keep it on the loopback behind the manager the page already talks to, rather than on the network. |
| `--port` | `0` | The port; `0` lets the system choose, and the lite says which on standard output. |
| `--max` | `8` | Listeners at once; the next is told `full` and closed. |
| `--idle` | `30` | Seconds without a listener after which the lite exits 0; `0` never. Counted from the start, so a lite started and never asked leaves too. |
| `--cap` | `0` | A sample rate above which the stream is resampled down to it and sent as 16-bit; `0` sends every stream as it is. |

The intended life is on demand: the manager starts the lite when the first page asks for the sound, reads the `listening` line for the port, proxies the page's WebSocket to it, and lets it leave by itself when nobody has listened for `--idle` seconds. Starting it with the player and `--idle 0` works too. `SIGTERM` ends it at once; it holds nothing that needs a clean close.

Standard output carries one JSON line per event and nothing else, so the software that started it can follow along:

```json
{"type":"listening","address":"127.0.0.1:41234","tag":"glass"}
{"type":"segment","rate":44100,"channels":2,"format":"s16le","bits":16,"frame_bytes":4,"one_bit":false,"capped":false,"chunk_ms":20}
{"type":"listeners","count":1}
{"type":"end"}
{"type":"listeners","count":0}
{"type":"exit","reason":"idle","seconds":30}
```

Standard error carries its log lines, each prefixed `outcast-lite:`.

## The wire

A client opens a WebSocket to the lite (any path) and sends nothing; pings are answered. The lite sends text frames, each one JSON object with a `type`, and binary frames, each a piece of the stream.

| Text frame | When | Fields |
| --- | --- | --- |
| `hello` | First, on connect | `version`, the lite's; `tag`. |
| `segment` | A stream begins, and on connect while one is on | `rate` in Hz; `channels`; `format`, one of `s16le`, `s24le`, `s32le`, `f32le`, interleaved and little-endian; `bits`, 16, 24 or 32; `frame_bytes`, one frame of every channel; `one_bit`; `capped`; `chunk_ms`. |
| `end` | The stream ended: the player stopped, or changed its format, in which case a new `segment` follows | |
| `gap` | The client fell behind and frames were dropped for it | `dropped`, how many. Reset the buffer and go on. |
| `full` | Too many listeners; the connection closes | |

Binary frames carry whole PCM frames of the segment's shape, at most `chunk_ms` of audio each, in order and at the player's own pace, since they are sent as the player writes them. Nothing is sent while the player stands still, nor between `end` and the next `segment`; a page that wants silence then makes its own.

`s24le` is three bytes a sample; `s16le`, `s32le` and `f32le` are what their names say. The level is the player's before its software volume, where the tap sits: a page should offer its own gain.

Frames a client has not taken are held for it up to two seconds, then dropped and counted; a client that takes no bytes for three seconds is closed.

## The rate cap

With `--cap 48000`, a stream above 48 kHz is resampled in the lite with a windowed sinc (`rubato`, the resampler the Outcast core uses) and sent as `s16le` at the cap, `capped` true; a stream at or below the cap passes untouched. The cap is for a phone on weak Wi-Fi or a television; the stream the player hears is never touched.

| Stream | On the wire, stereo |
| --- | --- |
| 44.1 kHz, 16-bit | 1.4 Mbit/s |
| 48 kHz, 24-bit | 2.3 Mbit/s |
| 96 kHz, 24-bit | 4.6 Mbit/s |
| 192 kHz, 24-bit | 9.2 Mbit/s |
| any, capped at 48 kHz | 1.5 Mbit/s |

## Playing it in a page

Web Audio takes PCM straight. Decode each binary frame to one `Float32Array` per channel, put them into an `AudioBuffer` at the segment's rate and schedule it on the context's clock right after the previous one, keeping a jitter buffer of 150 to 500 ms ahead of the clock; the browser resamples to the device's rate. `AudioBufferSourceNode` scheduling is in every browser that has Web Audio; an `AudioWorklet` with a ring buffer is steadier under load where it exists (Chromium 66, Firefox 76, Safari 14.1 and later) and takes the same frames. iOS wants the context resumed inside a tap on the page.

A sketch of the simple path, enough to hear it:

```js
const ctx = new AudioContext();            // resumed inside a tap on the page
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
             : seg.format === 's24le' ? (v.getUint8(p) | (v.getUint8(p + 1) << 8) | (v.getInt8(p + 2) << 16)) / 8388608
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

On a phone the sound stops when the screen locks or the tab goes to the background, since a page's audio graph is not media to the system. Route the graph into a media stream and an audio element instead of the context's destination, and give it a media session: the sound then goes on with the screen off, with the lock screen's controls, on Android Chrome and iOS Safari alike. In place of `gain.connect(ctx.destination)` above:

```js
const dest = ctx.createMediaStreamDestination(); gain.connect(dest);
const el = new Audio(); el.srcObject = dest.stream; el.play();   // inside the same tap that resumed the context
if ('mediaSession' in navigator) navigator.mediaSession.metadata = new MediaMetadata({ title: 'Volumio' });
```

## Behind a manager

The page talks to its manager already; the lite listens on the loopback. Node proxies a WebSocket upgrade socket to socket with no library, so the page sees one origin, one port and the manager's own access control:

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

The lite's answer to the upgrade goes back through the same socket. `litePort` is what the `listening` line said.

## Playing it in a native remote

A remote display on Linux, Windows or Android is a native client of the same wire: it opens the WebSocket the page opens, through the player's manager at the route the manager gives it (`ws://<player>:<manager port>/anymote/listen`, with whatever the manager asks of a remote), and takes the same frames. The lite never listens on the network itself; the manager's route is the way in from another machine.

What the client does with the frames, in the order the stream brings them:

1. **`segment`**: open the audio device at the segment's rate and channels, in the widest format the device takes, and convert each sample from the segment's format on the way (`s16le` as it is; `s24le` three bytes sign-extended; `s32le` as it is or its top bits; `f32le` as it is). A new `segment` while one is open: stop, clear the queue, reopen at the new shape. `one_bit`: say "plays on the player only" and queue nothing.
2. **Binary frames**: queue them on the device. Start the device once about 200 ms are queued, not before, so the first frames do not run dry; that lead is the jitter buffer, and a quarter second is right for a LAN.
3. **`gap`** and **`end`**: clear the queue; the stream starts again from the next frame (`gap`) or the next `segment` (`end`).
4. **The queue's depth over time**: the player's clock and the device's clock drift by a few milliseconds a minute. If the queued audio grows past about half a second, drop it back to the lead; if it runs dry, pause the device until the lead is there again. A remote that resamples (the device at its own rate, the stream through a resampler such as `rubato`) can trim the ratio by a hair instead of dropping; either is inaudible.
5. **The socket closing**: the lite leaves when nobody has listened for its idle time, and the manager starts it again on the next ask; reconnect with a backoff of a second or two.

With SDL2, which the Glass remote already carries, this is an `AudioQueue` opened with `AudioSpecDesired { freq: rate, channels, samples: None }`, `queue_audio` per frame, `size()` for the depth, `clear()` on gap and end, `resume()` once the lead is queued. On Linux a remote can open the ALSA device directly at the stream's own rate and width and be bit-perfect to its DAC, the one place on the way where that is possible; on Android (AAudio) and Windows (WASAPI) the system mixer resamples to the device's rate, which loses nothing but the bits.

### The client crate

The wire's client side is a crate of the outcast workspace, `lite-client`, pure Rust, for a remote to link on Linux, Windows and Android alike: the connection, every frame as an event, the segment's shape, and the PCM of each frame decoded to 16-bit or float for the audio device. A remote takes it by tag, as Outcast takes Glass's tap crate:

```toml
[dependencies]
lite-client = { git = "https://github.com/foonerd/outcast", tag = "v0.1.6" }
```

```rust
use lite_client::{Client, Event};
let mut client = Client::connect("ws://player:5582/anymote/listen")?;
let (mut shape, mut samples) = (None, Vec::new());
loop {
    match client.event()? {
        Event::Segment(s) => shape = Some(s),           // open the device at s.rate, s.channels
        Event::Pcm(pcm) => if let Some(s) = &shape {
            s.to_f32(&pcm, &mut samples);               // or to_i16; then queue them
        },
        Event::Gap { .. } | Event::End => {}            // clear the device's queue
        Event::Closed => break,                         // reconnect after a moment
        _ => {}
    }
}
```

A later lite's new frame types come back as `Event::Other` and its new fields are ignored, so a remote built against one version runs against the next. The crate builds for the Android ABIs with cargo-ndk exactly as the Glass app does; outcast's CI builds it and the reference client for arm64-v8a, armeabi-v7a and x86_64 on every commit, with the same NDK and the same steps as Glass's own Android job, so a remote that links it has nothing new to set up.

The reference client ships in every archive and in the repository as `examples/lite-client.rs`: it uses the crate, prints every event to standard error and writes the PCM to standard output as it comes, or as 16-bit with `--s16`, so a remote's audio code can be tried against the real wire before it is written:

```sh
cargo build -p lite-client --example lite-client
target/debug/examples/lite-client ws://<player>:5582/anymote/listen --s16 | aplay -f S16_LE -r 44100 -c 2
```

The rest of a remote is the queue and the rules above.

## Versions and the contract

The lite's version is said in `hello`. The wire is a contract, as the ring is: a field is never renamed or removed within a major version, and additions come as new fields and new `type`s a client may ignore. The `segment` fields above are the first version's whole set.

## Building it

The lite is built from the Outcast workspace: `cargo build --release -p outcast-lite` for this machine, `scripts/ship.sh` for every player architecture (static, musl), `scripts/package-lite.sh` for the archives; the release workflow signs and publishes them. `crates/stream` is what the lite and the Outcast core share: the segment, the feed from the ring and the conformer behind the cap.

## Licence

MIT. Outcast Lite is part of the [evo framework](https://evoframework.org), designed, built and maintained by one developer in his own time; sponsorship of the framework through GitHub Sponsors or Ko-fi is a thank-you, not a purchase.

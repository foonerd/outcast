# Outcast Uplink

A Volumio plugin of the [evo framework](https://evoframework.org) that casts whatever the player plays out to any uplink: an Icecast or SHOUTcast server, an RTMP or SRT ingest, an HLS origin, a stream on the LAN, or a file. Every service the player has is carried, since the sound is taken from the player's audio chain by [evotap](https://github.com/foonerd/evotap) rather than from one player program; the stream's title is what the player's screen shows; and the player never waits on an uplink, so a slow or absent server costs playback nothing.

Each uplink has its own encoder, transport, reconnection and status. One failing never stops another.

The first release carries three uplinks: an Icecast server, a stream on the network, and recording to files. The radio (SHOUTcast), broadcast (RTMP, SRT) and CDN (HLS) families follow, each with its own release.

## The uplinks

| Uplink | What it does |
| --- | --- |
| Icecast | The source protocol to one mount of an Icecast server (AzuraCast, Zeno, Live365, Radio.co and most hosted radio take the same), as MP3, AAC, Opus or Ogg Vorbis. The mount stays up with silence while the player pauses or stops, from the moment the uplink is on, unless set to follow the player. A server that goes away is retried with a backoff; a server that stops taking bytes is found within seconds and dropped; a refused password is retried after a minute. The title the player shows is pushed to the server for MP3 and AAC and carried in the stream for Opus and Ogg Vorbis. |
| The stream on the network | An HTTP stream on a port of the player, at `http://<player>:<port>/`, as MP3, AAC, Opus, Ogg Vorbis or FLAC, for any player on the network: it goes on with silence while the player pauses or stops, unless set to end with it; titles in band for a player that asks for them; a listener joining late given the stream's headers; a limit on listeners. |
| Recording | FLAC, WAV, MP3, AAC, Opus or Ogg Vorbis files under a folder of the player (`/data/INTERNAL/outcast/recordings` unless set otherwise, reachable over the player's network share), a file per track named by the moment and the title, or one file per stream. |

Each is set up on the plugin's settings page; the status button there says what the player is playing as the tap sees it and where each uplink stands.

## Installing and the audio chain

The plugin installs from the Volumio plugin store, or from a release here through the player's plugin manager. On enabling, the tap is proved before it is taken into the player's audio chain: the player's own ALSA loads it and plays a second through it into nothing; only then is its contribution registered, and the chain as written is verified. Any failure takes the contribution back and says why, so a player is never left with a chain it cannot open. Disabling the plugin takes the tap out again. Nothing of the player's own software is modified; every binary the plugin runs is in its own folder.

## How it is built

The plugin is three parts in one zip, one copy of each binary per Volumio architecture (`arm`, `armv7`, `armv8`, `x64`), nothing of the player's own software relied on:

| Part | What | Where it comes from |
| --- | --- | --- |
| The shell (`plugin/`) | The Volumio plugin as the store wants it: the settings page, the install with the tap's ALSA contribution proved before it is registered and taken back when the chain does not take it, the title from the player's state, the core's service. | This repository. |
| The core (`bins/outcast`) | One daemon in Rust, started by the shell as its own systemd unit: it follows the audio ring, carries the sound to every uplink, runs each uplink's transport, keeps a status file and answers a control socket. Static, linked against musl. | This repository, cross-built in CI. |
| The components | [evotap](https://github.com/foonerd/evotap), the ALSA plugin the sound passes through and the ring it writes; [evopeg](https://github.com/foonerd/evopeg), FFmpeg built down to encoding and muxing from a pipe to a pipe. | Their own releases, at the versions `components/*.lock` name, fetched in CI and verified against the evo release key (`keys/release-signing.pub`) before anything of them is packed. |

Every release is built by GitHub Actions from a tag: the check, the cross builds, the components, the zip with `MANIFEST` and its Ed25519 signature inside and `SHA256SUMS` with its signature beside, published as a pre-release and promoted by hand once tried. `build.json` in the zip names the commit; `components.json` names each component's version and every file taken from it with its digest.

To work on it: `scripts/check.sh` is the workshop pass (format, lints, tests, docs, the shell scripts, the ALSA template, the plugin's files and tests), `scripts/ship.sh` the cross builds, `scripts/fetch-components.sh` the components, `scripts/package.sh` the zip. The Rust toolchain is pinned by `rust-toolchain.toml`; Node is needed only for the plugin's checks.

## Licence

MIT. Outcast Uplink is part of the evo framework, designed, built and maintained by one developer in his own time; sponsorship of the framework through GitHub Sponsors or Ko-fi is a thank-you, not a purchase.

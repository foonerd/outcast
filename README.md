# Outcast Uplink

A Volumio plugin of the [evo framework](https://evoframework.org) that casts whatever the player plays out to any uplink: an Icecast or SHOUTcast server, an RTMP or SRT ingest, an HLS origin, a stream on the LAN, or a file. Every service the player has is carried, since the sound is taken from the player's audio chain by [evotap](https://github.com/foonerd/evotap) rather than from one player program; the stream's title is what the player's screen shows; and the player never waits on an uplink, so a slow or absent server costs playback nothing.

Each uplink has its own encoder, transport, reconnection and status. One failing never stops another.

There is no release yet. The first will carry the Icecast uplink, the LAN stream and recording to a file; the radio, broadcast and CDN families follow, each with its own release.

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

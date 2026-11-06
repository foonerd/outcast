# Changelog

All notable changes to Outcast Uplink. The format follows Keep a Changelog; the versions follow Semantic Versioning.

## [0.1.10] - 2026-11-06

### Added

- `rtmps://` for the RTMP uplink: the session inside TLS, the server's certificate checked against the public authorities, or against the operator's own authority's PEM file (an Advanced setting) for a private media server; the TLS pure Rust (rustls). Platforms that take only RTMPS (Facebook Live) are reachable; a bogus key on a public platform is now reported as the server closing at the publish, as it does.

## [0.1.9] - 2026-11-06

### Added

- The RTMP uplink: the stream published to an RTMP ingest (a streaming platform, a media server, a CDN) with the address and the stream key the platform gives, as AAC or MP3, a preset or the operator's own; the session pure Rust. The stream key is never shown or logged. A server that stops reading is found by the acknowledgements it stops sending, within seconds even while the player stands still, and the stream is published again with a backoff; silence keeps it published while the player pauses or stops, unless set to follow the player.
- The transport-stream uplink: the stream as an MPEG transport stream straight to a receiver over TCP (the player connecting to a listener; a receiver that stops taking bytes is found and the connection made again) or over UDP (datagrams to a host or a multicast group, with the hop count set); AAC, MP2, MP3 or Opus in the stream, a preset or the operator's own.

## [0.1.8] - 2026-11-06

### Added

- The SRT uplink: the stream as an MPEG transport stream over SRT to a listener (a production switcher, a broadcast encoder, a gateway, a server), this player the caller, with a stream id, a passphrase for an encrypted link and the link's latency as the listener needs them; AAC, MP2, MP3 or Opus in the stream, a preset or the operator's own. The transport is the core's own, pure Rust. A link that goes away is made again with a backoff; the link is kept up with silence while the player stands still unless set to follow it; the status says the round trip and the packets lost.
- MP2, the broadcast transport stream's own codec, for the SRT uplink.

### Changed

- Every section of the page shows its switch alone while it is off: the mount point, the source user and the stream number sit behind the uplink's switch (their words say which server type each is for), the programme watch has a switch of its own, and a field behind a nested switch (Advanced, More settings) is hidden while its section is off.

## [0.1.7] - 2026-11-06

### Added

- Three Icecast or SHOUTcast destinations at once: the page has Icecast 2 and Icecast 3 beside the first, each with its own settings, encoder, connection and status; one failing never stops another. The core takes any number.

## [0.1.6] - 2026-11-04

### Added

- The programme's level and dead air in the status: the peak and RMS per channel from the tap's measurement ring, and dead air when the programme has been below the dead-air level for the dead-air time, whether the player plays silence or plays nothing; the core's log says when dead air begins and ends. The page has the two settings, a level in dBFS (-60 unless set) and a time in seconds (30 unless set; 0 never).
- The core's unit runs at `Nice=5`, behind the player's audio thread.

## [0.1.5] - 2026-11-03

### Added

- outcast-lite, beside the plugin zip on every release: one small static binary per player architecture that reads the tap's ring and serves the player's sound as it is, PCM over a WebSocket, no encoder, for a page or a remote that wants to hear the player where the person is. A text frame says what the stream is, binary frames of at most 20 ms carry it; a rate cap sends a hi-res stream down at 16-bit for a weak link; the port is said on standard output and the process leaves by itself when nobody has listened for a while. Signed like every evo component.
- The lite's client for every remote platform, `outcast-lite-client-<version>-<platform>.tar.gz` for linux-x64, linux-armv7, linux-armv8, windows-x64 and android: the reference client built for the platform, the `lite-client` crate's source that a remote links on Linux, Windows and Android (the connection, every frame as an event, the segment's shape, the PCM decoded to 16-bit or float), the document and the licence. The crate is built in CI for the three Android ABIs the way Glass builds its app.
- `docs/outcast-lite.md`, inside every archive as its README: what the lite is and is not, getting and verifying it, what it needs on the player, running it, the wire, the rate cap, playing it in a page (with the screen locked too) and in a native remote, the proxy behind a manager, the contract.

### Changed

- The segment, the feed and the conformer are a crate of their own (`crates/stream`), shared by the core and the lite; the core is as it was.

## [0.1.4] - 2026-11-03

### Changed

- The destination decides: the Icecast and network stream sections take any bitrate, any sample rate or "as played", and stereo, mono or as played, with the values commonly used for the chosen server type as defaults, a Suggested settings button that fills them in, and the words beside each field saying what the common servers take. Nothing is refused by the plugin; a server's or the encoder's own refusal is shown as it comes. SHOUTcast takes any codec the operator sets.

### Added

- A sample rate that is set gives one continuous stream at that rate whatever the player plays: a track at another rate is resampled in the core (a windowed sinc, rubato), one at another width is widened, a mono track is doubled, and the encoder runs on across the player's changes. "As played" follows the player's rate and starts the stream anew at every change, as before.
- The core's log says what it does with each stream: as it is, resampled to the chosen rate, or widened.

## [0.1.3] - 2026-11-03

### Added

- SHOUTcast servers: the Icecast uplink has a server type, Icecast 2 as before or SHOUTcast, for a SHOUTcast DNAS of version 1 or of version 2 in its legacy mode. With SHOUTcast chosen the page asks for the server's own port and a stream number (1 unless the DNAS has several) in place of the mount point and the source user; the stream is MP3 or AAC; the title goes to the server through its admin interface. The connection, the reconnection, the stall watch and the silence that keeps the stream up are the same as for Icecast.
- The settings page in German, Spanish, French, Italian, Japanese, Dutch, Polish, Portuguese, Russian and Chinese beside English.

## [0.1.2] - 2026-11-03

### Added

- The mount and the network stream stay up while the player pauses or stops: the uplink fills with silence at the stream's shape, paced by the clock, from the moment it is on and between streams, so a server never times the source out and a listener stays connected. A stream of the same shape as the last goes on through the same encoder without a seam. Each uplink has a switch to follow the player instead, as before.
- The status says when an uplink carries silence.

## [0.1.1] - 2026-11-03

### Changed

- evotap 0.1.1, built from Glass 0.11.10: the tap's measuring path now takes the meters' lead ahead of the sound from the demand file; the audio ring, the sample layouts, the readers and the contract are as they were. Nothing of the uplinks changed.

## [0.1.0] - 2026-11-03

### Added

- The first release: the Icecast uplink, the stream on the network and recording to files, each with its own encoder, transport and status, fed from the player's audio chain by evotap so that every service the player has is carried, with the title the player's screen shows.
- The Icecast uplink speaks the source protocol itself: reconnects with a backoff after a server that went away, a minute's wait after a refused password, a server that stops taking bytes found within seconds and dropped, the title pushed to the server for MP3 and AAC and carried in the stream for Opus and Ogg Vorbis; a format change between tracks is one connection.
- The stream on the network: an HTTP stream on a port of the player, MP3, AAC, Opus, Ogg Vorbis or FLAC, titles in band for a player that asks for them, a listener joining late given the stream's headers, a limit on listeners.
- Recording: FLAC, WAV, MP3, AAC, Opus or Ogg Vorbis files under a folder of the player, a file per track named by the moment and the title, or one per stream.
- The audio path proved before it is taken: the tap is loaded by the player's own ALSA and played through into nothing before its contribution is registered, the chain is verified after, and any failure takes the contribution back and says why.
- The core, evotap and evopeg shipped for every architecture in one zip, each component verified against the evo release key when the zip is made; `components.json` names what went in.

# Changelog

All notable changes to Outcast Uplink. The format follows Keep a Changelog; the versions follow Semantic Versioning.

## [0.1.5] - 2026-11-03

### Added

- outcast-lite, beside the plugin zip on every release: one small static binary per architecture that reads the tap's ring and serves the player's sound as it is, PCM over a WebSocket, no encoder, for a page or a remote that wants to hear the player where the person is (Glass's Anymote first). A text frame says what the stream is, binary frames of at most 20 ms carry it; a rate cap sends a hi-res stream down at 16-bit for a weak link; the port is said on standard output and the process leaves by itself when nobody has listened for a while. Signed like every evo component, with `docs/outcast-lite.md` for whoever takes it.

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

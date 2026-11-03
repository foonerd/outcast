# Changelog

All notable changes to Outcast Uplink. The format follows Keep a Changelog; the versions follow Semantic Versioning.

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

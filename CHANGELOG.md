# Changelog

All notable changes to Outcast Uplink. The format follows Keep a Changelog; the versions follow Semantic Versioning.

## [0.1.23] - 2026-11-08

### Added

- Insert files from a browser: the programme page uploads a jingle or a recording from the device it is open on, a phone included, and the core converts it to the WAV the mixer plays as it lands (AAC in M4A, MP3, FLAC, Ogg Vorbis or WAV, 256 MB at most); the files are listed with their length, used as inserts with a click, and deleted from the page. A recording made on the road reaches the stream with nothing but the player's address.
- `outcast convert IN OUT.wav` on the command line, the same conversion by hand.

## [0.1.22] - 2026-11-08

### Fixed

- The dashboard and the status could show nothing of DJ mode: the plugin told the core once per change, and when the first word went while the core was still starting, or the core restarted later, nothing was said again. The plugin now tells the core again whenever the core's status lacks DJ mode.

## [0.1.21] - 2026-11-08

### Added

- DJ mode, the second stage: inserts. Jingles, station IDs and announcements as WAV files in the player's jingles folder, played into the stream over the programme by the core's mixer, the programme ducked under them (a bed, under, well under, or cut) and brought back after, the insert's edges faded; fitted to the stream's rate and channels as they play. When: every hour at a minute, every so many tracks, or at each block's start; soft at the end of the track playing (five minutes at most where none ends) or on the clock; or now, from the programme page. The room's audio is untouched: the inserts go to the uplinks and the recording alone. The status says the insert playing, the Since card counts them, and a one-bit stream, which is not mixed, says so.

## [0.1.20] - 2026-11-08

### Added

- DJ mode, the first stage: the programme as blocks over the day. Each block names a start time and a source the player has, its own queue, a playlist on random or in order, or a web radio, and whether it starts at the end of the track playing or on the clock; the blocks repeat daily, the last running over midnight into the first. At a block's start the player is told what to play; a queue changed by hand makes DJ mode stand back until the next block, and a take-over hands it back. The programme page, beside the dashboard at `/dj` on its port, is where the blocks are set and where what is on now and next is shown; the Volumio page has the switch and the buttons that open it. The dashboard's Programme card says the block in force, the next, and when DJ mode stands back; a block's start is an event in the Since card. Jingles and inserts come in a later stage.

### Changed

- The release build fetches the components with more patience, and through GitHub's API when the download addresses fail, as they did for an hour on 8 November.

## [0.1.19] - 2026-11-08

### Added

- A Since card on the dashboard: what happened since the core started or since the card's Clear: dead-air events with their time in all and the last one, connections made again in all and by uplink, errors with the last one, the peaks of CPU, Outcast's share, temperature and listeners, bytes carried and the uplinks' average rate, bytes lost and pieces dropped, meter hops at full scale, and the last events with their times (dead air and back on air, an uplink lost and back, an error, the uplinks' start, the clearing). Clear starts the counts again for every viewer; it is the page's one write, and it touches nothing else. One line of it under the status button.
- The Level card as a meter bridge across the page: left and right on a scaled trough with the zones marked, the instant peak, a held peak that falls after three seconds and turns red at full scale, the dead-air floor as set; readouts for the peak hold, RMS over ten seconds, the highest peak of the last minute, balance, overs and the floor; and the last minute as a graph of RMS left and right with the peaks above them.
- A Played card: the titles the player showed with their start times and how long each ran, the current one on top with its running time; twenty are kept in the status.
- A clock in the dashboard's header.

### Changed

- An encoder's cost below the meter's step prints as "<0.1 %" rather than "0.0 %".

## [0.1.18] - 2026-11-08

### Added

- The player on the dashboard: a Player card with the whole machine's CPU and load averages, Outcast's own share with its encoders', memory used and Outcast's own, the processor's temperature where the player has a sensor (amber from 70 °C, red from 80), and what goes out: the uplinks' total and the player's interface. Each uplink's row says what its encoder costs. Read once a second from what the kernel keeps, no process run; in the status as `system`, and one line of it under the status button.

### Fixed

- The recording folder could not be anywhere but under `/data/INTERNAL/outcast`: the core's unit kept the rest of the file system read-only, so a mounted share, which the page offered, ended in an error. The unit now leaves `/data`, `/mnt` and `/media` writable (`/usr`, `/boot` and `/etc` stay read-only).

## [0.1.17] - 2026-11-07

### Added

- A way out of the dashboard. The status section now has two buttons: "Open the dashboard here" shows the page inside Volumio's own page with its arrow back, so the player's own display and the app can leave it; "Open the dashboard in a new tab" is for a second screen or another device, its address written under the button. The page itself carries a Close button when it is the window: the window closes where the browser allows, else the page steps back, else it goes to the player's own page.
- The dashboard's address, beside its port: empty uses the address found on the player's own network interface, which works where `.local` names do not resolve; a host name or an IP address of the operator's when the player is reached another way.

### Changed

- The dashboard's look: the same palette and parts as the Glass manager, for a studio wall: a near-black ground, graphite panels with titled headers, green, amber and red state badges, a Programme card with an on-air badge, a Level card with ticked meters, one row per uplink with name, state, address and counters, and "n of m up" in the uplinks' title.

## [0.1.16] - 2026-11-07

### Added

- The dashboard: a page of the core's own, served from its memory on a port of the player (8020 unless set, switched off on the page when it is not wanted), for watching the programme, the level with its meters, dead air and every uplink's state, counters and last error from anything on the network, a second at a time. Nothing is fetched from anywhere else; the page is one file built into the core. The status section carries the button that opens it at the player's address.

## [0.1.15] - 2026-11-07

### Fixed

- A section switched off showed the fields behind its nested controls (the HLS origin's address and keys, the segment form's DASH switch, the port behind "serve on the player") as if they belonged nowhere; now a section off shows its switch alone, whatever its nested selects and switches hold.

## [0.1.14] - 2026-11-07

### Added

- The HLS server holds a set number of connections at once (64 unless set, 1 to 1000, beside the port on the page); the rest are refused at once with 503, so a flood never grows the player's threads without bound. The status says the connections open and the requests refused.
- The status says whose library serves the tap when another plugin's copy does (ALSA takes the last `pcm_type.evotap` in the chain): the same library, or a different build with the core's ring reader deciding.
### Fixed

- The transport-stream uplink kept its encoder running after losing the receiver, so the encoder's pipe filled and the worker stood still in it, and a save on the page then waited on that worker for ever: the whole core froze. The encoder now goes with the connection and comes back with the next one, as in every other uplink.
- A worker of a reload still ending could write its last words over the new worker's status entry; entries and workers now carry the reload's generation, and a worker that does not stop within ten seconds is left behind instead of holding the core.

## [0.1.13] - 2026-11-07

### Added

- HLS in fragmented MP4 (CMAF), from evopeg 0.2.0: AAC, FLAC (lossless HLS), MP3 or Opus in fragments of the segment's length, the init segment once per timeline, the playlist at version 7 with its map, the title as an `emsg` timed-metadata box when it changes; the segment form is the operator's choice, packed audio staying the default.
- DASH beside the playlist, from the same fragmented segments: a dynamic manifest with a period per timeline, the segments as a timeline under a template by number, served on the player and put on the origin.

### Changed

- evopeg 0.2.0 (the `mp4` muxer; FFmpeg 9.0.2).

## [0.1.12] - 2026-11-06

### Added

- An S3-compatible bucket as the HLS origin (AWS S3, Cloudflare R2, Backblaze B2, a server of the operator's): the endpoint, the bucket, a folder in it, the region, the keys, the bucket in the path or as the host's first label; every request signed (Signature Version 4, pure Rust), each object put with its content type and the caching a CDN wants (the playlist uncached, the segments an hour), the stale ones removed. The secret is never shown or logged.

## [0.1.11] - 2026-11-06

### Added

- The HLS uplink: the stream as HTTP Live Streaming, packed audio (AAC or MP3) cut into segments of a few seconds with a sliding playlist, each segment tagged with its timestamp and the title; the segments and the playlist in the core's memory, served on a port of the player and put on the operator's own HTTP origin (PUT and DELETE, `http://` or `https://`, basic authentication, a private authority's file) by an uploader that never holds the stream up. The playlist on the origin only ever names segments that landed; an origin that goes away is tried again with a backoff while the player's own playlist goes on; the status says the segments cut and those waiting for the origin. The segment numbers and the timelines run on across saves on the page, so a player mid-stream sees a discontinuity, never a restart.

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

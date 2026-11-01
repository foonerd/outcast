# Outcast Uplink

A Volumio plugin of the [evo framework](https://evoframework.org) that casts whatever the player plays out to any uplink: an Icecast or SHOUTcast server, an RTMP or SRT ingest, an HLS origin, a stream on the LAN, or a file. Every service the player has is carried, since the sound is taken from the player's audio chain by [evotap](https://github.com/foonerd/evotap) rather than from one player program; the stream's title is what the player's screen shows; and the player never waits on an uplink, so a slow or absent server costs playback nothing.

Each uplink has its own encoder, transport, reconnection and status. One failing never stops another.

There is no release yet. The first will carry the Icecast uplink, the LAN stream and recording to a file; the radio, broadcast and CDN families follow, each with its own release.

## Licence

MIT. Outcast Uplink is part of the evo framework, designed, built and maintained by one developer in his own time; sponsorship of the framework through GitHub Sponsors or Ko-fi is a thank-you, not a purchase.

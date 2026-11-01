'use strict';
// The plugin's pure parts, with no dependency on the player: what the tests exercise
// and index.js uses. Nothing here reads a file or talks to the player.

// The title as the player's screen shows it: "Artist - Title", or whichever is there.
function screenTitle(state) {
    if (!state) return '';
    const title = String(state.title || '').trim();
    const artist = String(state.artist || '').trim();
    return artist && title ? artist + ' - ' + title : (title || artist);
}

// The ALSA configuration the preflight plays through: the player's own base
// configuration, the tap's library, and one PCM of the tap writing into nothing. The
// player's chain is not in it.
function checkConfig(library, tag, base) {
    return [
        '<' + (base || '/usr/share/alsa/alsa.conf') + '>',
        'pcm_type.evotap {',
        '  lib ' + library,
        '}',
        'pcm.OutcastCheck {',
        '  type evotap',
        '  slave.pcm "null"',
        '  ring "' + tag + '"',
        '  audio 1',
        '}',
        ''
    ].join('\n');
}

// Whether the player's ALSA file, as written, carries the tap.
function chainHasTap(text) {
    return /^pcm\.Outcast\s*\{/m.test(String(text || ''));
}

// The status as one toast: the core, the stream, the tap, the uplinks. `t` translates
// a key with its variables.
function summarize(status, audioPath, audioPathError, t) {
    const lines = [];
    if (!status) {
        lines.push(t('STATUS_CORE_DOWN'));
    } else if (status.ring) {
        lines.push(t('STATUS_PLAYING', {
            rate: status.ring.rate,
            channels: status.ring.channels,
            format: status.ring.format,
            segments: status.segments,
            mb: (status.bytes / 1048576).toFixed(1)
        }));
    } else {
        lines.push(t('STATUS_IDLE'));
    }
    if (audioPath === 'ready') lines.push(t('STATUS_TAP_READY'));
    else if (audioPath === 'lost') lines.push(t('STATUS_TAP_LOST'));
    else lines.push(t('STATUS_TAP_FAILED', { error: audioPathError || '' }));
    if (!status || !Array.isArray(status.uplinks) || status.uplinks.length === 0) {
        lines.push(t('STATUS_NO_UPLINKS'));
    } else {
        status.uplinks.forEach((u) => lines.push(u.id + ': ' + u.state));
    }
    return lines;
}

// A string of the plugin's dictionary with its {{variables}} filled.
function fill(text, vars) {
    let s = String(text);
    Object.keys(vars || {}).forEach((k) => { s = s.split('{{' + k + '}}').join(String(vars[k])); });
    return s;
}

module.exports = { screenTitle, checkConfig, chainHasTap, summarize, fill };

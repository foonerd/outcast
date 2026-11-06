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

// A string of the plugin's dictionary with its {{variables}} filled.
function fill(text, vars) {
    let s = String(text);
    Object.keys(vars || {}).forEach((k) => { s = s.split('{{' + k + '}}').join(String(vars[k])); });
    return s;
}

// The value of a select field as the page sends it, or a plain value.
function sel(v) {
    return v && typeof v === 'object' && 'value' in v ? String(v.value) : String(v === undefined || v === null ? '' : v);
}

const num = (v, fallback) => {
    const n = parseInt(String(v).trim(), 10);
    return Number.isFinite(n) ? n : fallback;
};

// ------------------------------------------------------------------ validation
// Each returns the message to show, or null. `t` translates a key with its variables.

function checkPort(v, field, t) {
    const s = String(v === undefined || v === null ? '' : v).trim();
    const n = parseInt(s, 10);
    return /^\d+$/.test(s) && n >= 1 && n <= 65535 ? null : t('ERR_PORT', { field: t(field) });
}

function checkRange(v, field, min, max, t) {
    const s = String(v === undefined || v === null ? '' : v).trim();
    const n = parseInt(s, 10);
    if (!/^\d+$/.test(s)) return t('ERR_NUMBER', { field: t(field) });
    return n >= min && n <= max ? null : t('ERR_RANGE', { field: t(field), min, max });
}

function checkText(v, field, t) {
    return /["\r\n]/.test(String(v || '')) ? t('ERR_TEXT', { field: t(field) }) : null;
}

// The Icecast uplink speaks Icecast 2 or SHOUTcast v1; the second has no mount and
// no user, a stream number instead.
function protocolOf(data, prefix) {
    return sel(data[prefix + '_protocol']) === 'shoutcast1' ? 'shoutcast1' : 'icecast2';
}

// The Icecast slots on the page, each a section of its own with its fields prefixed.
const ICE_SLOTS = ['ice', 'ice2', 'ice3'];

// ------------------------------------------------------------------ the encoding
// What an uplink sends is a preset, the settings commonly used for its kind of
// destination, or whatever the operator sets under Advanced: any codec evopeg makes, any
// bitrate, any sample rate (0 for the player's own), stereo, mono or the player's own
// channels. The plugin refuses nothing; the destination or the encoder does, and the
// status says so. A player upgraded from a release without presets keeps the values it
// had, as if Advanced were on.
const PRESETS = {
    'mp3-128-44100': { codec: 'mp3', bitrate: 128, rate: 44100, channels: 2 },
    'mp3-192-44100': { codec: 'mp3', bitrate: 192, rate: 44100, channels: 2 },
    'mp3-320-44100': { codec: 'mp3', bitrate: 320, rate: 44100, channels: 2 },
    'aac-96-44100': { codec: 'aac', bitrate: 96, rate: 44100, channels: 2 },
    'aac-128-48000': { codec: 'aac', bitrate: 128, rate: 48000, channels: 2 },
    'opus-64-48000': { codec: 'opus', bitrate: 64, rate: 48000, channels: 2 },
    'opus-96-48000': { codec: 'opus', bitrate: 96, rate: 48000, channels: 2 },
    'vorbis-128-44100': { codec: 'vorbis', bitrate: 128, rate: 44100, channels: 2 },
    'flac-48000': { codec: 'flac', bitrate: 0, rate: 48000, channels: 2 },
    'flac-0': { codec: 'flac', bitrate: 0, rate: 0, channels: 0 },
    'wav-0': { codec: 'wav', bitrate: 0, rate: 0, channels: 0 }
};
const DEFAULT_PRESET = { ice: 'mp3-128-44100', ice2: 'mp3-128-44100', ice3: 'mp3-128-44100', lan: 'mp3-128-44100', rec: 'flac-0' };

function checkWhole(v, field, t) {
    const s = String(v === undefined || v === null ? '' : v).trim();
    return /^\d+$/.test(s) ? null : t('ERR_NUMBER', { field: t(field) });
}

// The encoding's own fields checked, when Advanced is on: numbers are numbers.
function validateEncoding(data, prefix, t) {
    if (data[prefix + '_advanced'] !== true) return null;
    return checkWhole(data[prefix + '_bitrate'], 'BITRATE', t) ||
        checkWhole(data[prefix + '_rate'], 'RATE', t) ||
        null;
}

function applyEncoding(data, prefix, set) {
    const advanced = data[prefix + '_advanced'] === true;
    set(prefix + '_preset', PRESETS[sel(data[prefix + '_preset'])] ? sel(data[prefix + '_preset']) : DEFAULT_PRESET[prefix]);
    set(prefix + '_advanced', advanced);
    if (!advanced) return;
    set(prefix + '_codec', sel(data[prefix + '_codec']));
    set(prefix + '_bitrate', String(data[prefix + '_bitrate']).trim());
    set(prefix + '_rate', String(data[prefix + '_rate']).trim());
    set(prefix + '_channels', sel(data[prefix + '_channels']));
}

// The encoding for the core: the preset's values, or the operator's own.
function encodingFrom(get, prefix) {
    const preset = String(get(prefix + '_preset', ''));
    const advanced = get(prefix + '_advanced', false) === true || preset === '';
    if (!advanced) {
        const p = PRESETS[preset] || PRESETS[DEFAULT_PRESET[prefix]];
        return { codec: p.codec, bitrate: p.bitrate, rate: p.rate, channels: p.channels };
    }
    const p = PRESETS[DEFAULT_PRESET[prefix]];
    return {
        codec: String(get(prefix + '_codec', p.codec)),
        bitrate: num(get(prefix + '_bitrate', String(p.bitrate)), p.bitrate),
        rate: num(get(prefix + '_rate', String(p.rate)), p.rate),
        channels: num(get(prefix + '_channels', String(p.channels)), p.channels)
    };
}

function validateIcecast(data, t, prefix) {
    const p = prefix || 'ice';
    const f = (k) => data[p + '_' + k];
    if (f('enabled') !== true) return null;
    const shoutcast = protocolOf(data, p) === 'shoutcast1';
    return (!String(f('host') || '').trim() ? t('ERR_HOST') : null) ||
        checkPort(f('port'), 'ICE_PORT', t) ||
        (shoutcast && parseInt(String(f('port')).trim(), 10) === 65535 ? t('ERR_PORT', { field: t('ICE_PORT') }) : null) ||
        (!shoutcast && !/^\/\S*$/.test(String(f('mount') || '').trim()) ? t('ERR_MOUNT') : null) ||
        (shoutcast ? checkRange(f('stream_id'), 'ICE_STREAM_ID', 1, 999, t) : null) ||
        (!f('password') ? t('ERR_PASSWORD') : null) ||
        (!shoutcast ? checkText(f('user'), 'ICE_USER', t) : null) ||
        validateEncoding(data, p, t) ||
        checkText(f('name'), 'STREAM_NAME', t) ||
        (f('more') === true && (
            checkText(f('description'), 'ICE_DESCRIPTION', t) ||
            checkText(f('genre'), 'ICE_GENRE', t) ||
            checkText(f('url'), 'ICE_URL', t))) ||
        null;
}

function validateLan(data, t) {
    if (data.lan_enabled !== true) return null;
    return checkPort(data.lan_port, 'LAN_PORT', t) ||
        validateEncoding(data, 'lan', t) ||
        checkText(data.lan_name, 'STREAM_NAME', t) ||
        checkRange(data.lan_max_listeners, 'LAN_MAX', 1, 64, t) ||
        null;
}

// A signed number, as a level in dBFS.
function checkSigned(v, field, t) {
    const s = String(v === undefined || v === null ? '' : v).trim();
    return /^-?\d+(\.\d+)?$/.test(s) ? null : t('ERR_NUMBER', { field: t(field) });
}

function validateMonitor(data, t) {
    return checkSigned(data.mon_level, 'MON_LEVEL', t) ||
        checkWhole(data.mon_seconds, 'MON_SECONDS', t) ||
        null;
}

function applyMonitor(data, set) {
    set('mon_level', String(data.mon_level).trim());
    set('mon_seconds', String(data.mon_seconds).trim());
}

function validateRecord(data, t) {
    if (data.rec_enabled !== true) return null;
    const folder = String(data.rec_folder || '').trim();
    return (!/^\/\S/.test(folder) || /["\r\n]/.test(folder) ? t('ERR_FOLDER') : null) ||
        validateEncoding(data, 'rec', t) ||
        null;
}

// ------------------------------------------------------------- the settings kept
// What a save writes into the plugin's configuration: the page's data as plain values.

function applyIcecast(data, set, prefix) {
    const p = prefix || 'ice';
    const f = (k) => data[p + '_' + k];
    const put = (k, v) => set(p + '_' + k, v);
    const on = f('enabled') === true;
    put('enabled', on);
    if (!on) return;
    const shoutcast = protocolOf(data, p) === 'shoutcast1';
    put('protocol', shoutcast ? 'shoutcast1' : 'icecast2');
    put('host', String(f('host')).trim());
    put('port', String(f('port')).trim());
    if (shoutcast) put('stream_id', String(f('stream_id')).trim());
    else put('mount', String(f('mount')).trim());
    if (!shoutcast) put('user', String(f('user') || 'source').trim() || 'source');
    put('password', String(f('password')));
    applyEncoding(data, p, set);
    put('name', String(f('name') || '').trim());
    put('keep_alive', f('keep_alive') !== false);
    put('more', f('more') === true);
    if (f('more') === true) {
        put('description', String(f('description') || '').trim());
        put('genre', String(f('genre') || '').trim());
        put('url', String(f('url') || '').trim());
        put('public', f('public') === true);
    }
}

function applyLan(data, set) {
    const on = data.lan_enabled === true;
    set('lan_enabled', on);
    if (!on) return;
    set('lan_port', String(data.lan_port).trim());
    applyEncoding(data, 'lan', set);
    set('lan_name', String(data.lan_name || '').trim());
    set('lan_max_listeners', String(data.lan_max_listeners).trim());
    set('lan_keep_alive', data.lan_keep_alive !== false);
}

function applyRecord(data, set) {
    const on = data.rec_enabled === true;
    set('rec_enabled', on);
    if (!on) return;
    set('rec_folder', String(data.rec_folder).trim().replace(/\/+$/, '') || '/');
    applyEncoding(data, 'rec', set);
    set('rec_split', sel(data.rec_split));
    set('rec_title_in_name', data.rec_title_in_name !== false);
}

// The file the core reads, from the configuration: one object per uplink, in the
// core's own words (bins/outcast/src/config.rs).
// One Icecast slot for the core, from the configuration, by its prefix.
function icecastFrom(get, p) {
    const str = (k, d) => String(get(p + '_' + k, d));
    const bool = (k) => get(p + '_' + k, false) === true;
    return {
        enabled: bool('enabled'),
        protocol: str('protocol', 'icecast2') === 'shoutcast1' ? 'shoutcast1' : 'icecast2',
        host: str('host', ''),
        port: num(get(p + '_port', '8000'), 8000),
        mount: str('mount', '/volumio.mp3'),
        stream_id: num(get(p + '_stream_id', '1'), 1),
        user: str('user', 'source'),
        password: str('password', ''),
        ...encodingFrom(get, p),
        name: str('name', 'Volumio'),
        description: bool('more') ? str('description', '') : '',
        genre: bool('more') ? str('genre', '') : '',
        url: bool('more') ? str('url', '') : '',
        public: bool('more') && bool('public'),
        keep_alive: get(p + '_keep_alive', true) !== false
    };
}

function uplinksFrom(get) {
    const str = (k, d) => String(get(k, d));
    const bool = (k) => get(k, false) === true;
    return {
        icecast: icecastFrom(get, 'ice'),
        icecasts: ICE_SLOTS.slice(1).map((p) => icecastFrom(get, p)),
        lan: {
            enabled: bool('lan_enabled'),
            port: num(get('lan_port', '8000'), 8000),
            ...encodingFrom(get, 'lan'),
            name: str('lan_name', 'Volumio'),
            max_listeners: num(get('lan_max_listeners', '5'), 5),
            keep_alive: get('lan_keep_alive', true) !== false
        },
        record: {
            enabled: bool('rec_enabled'),
            folder: str('rec_folder', '/data/INTERNAL/outcast/recordings'),
            ...encodingFrom(get, 'rec'),
            split: str('rec_split', 'track'),
            title_in_name: get('rec_title_in_name', true) !== false
        },
        monitor: {
            dead_air_level: Number.isFinite(parseFloat(get('mon_level', '-60'))) ? parseFloat(get('mon_level', '-60')) : -60,
            dead_air_seconds: num(get('mon_seconds', '30'), 30)
        }
    };
}

// -------------------------------------------------------------------- the status
// The status as lines for one toast: the core, the stream, the tap, each uplink.

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
    if (status && status.level && Array.isArray(status.level.rms_db) && status.level.rms_db.length) {
        const top = (a) => Math.max(...a).toFixed(1);
        lines.push(t('STATUS_LEVEL', { peak: top(status.level.peak_db), rms: top(status.level.rms_db) }));
    }
    if (status && status.dead_air === true) lines.push(t('STATUS_DEAD_AIR', { seconds: status.quiet_seconds }));
    if (audioPath === 'ready') lines.push(t('STATUS_TAP_READY'));
    else if (audioPath === 'lost') lines.push(t('STATUS_TAP_LOST'));
    else lines.push(t('STATUS_TAP_FAILED', { error: audioPathError || '' }));
    const names = { icecast: 'UPLINK_ICECAST', lan: 'UPLINK_LAN', record: 'UPLINK_RECORD' };
    if (!status || !Array.isArray(status.uplinks) || status.uplinks.length === 0) {
        lines.push(t('STATUS_NO_UPLINKS'));
    } else {
        status.uplinks.forEach((u) => {
            const slot = /^icecast(\d+)$/.exec(u.id);
            const name = (names[u.kind] ? t(names[u.kind]) : u.id) + (slot ? ' ' + slot[1] : '');
            let detail = u.detail || '';
            if (u.kind === 'lan') detail += ', ' + u.listeners + ' listening';
            if (u.silence) detail += ', silence';
            if (u.kind === 'record' && u.files) detail += ', ' + u.files + ' files';
            if (u.title) detail += ', ' + u.title;
            if (u.last_error && (u.state === 'error' || u.state === 'waiting')) {
                lines.push(t('STATUS_UPLINK_ERROR', { name, state: u.state, error: u.last_error }));
            } else {
                lines.push(t('STATUS_UPLINK', { name, state: u.state, detail }));
            }
        });
    }
    return lines;
}

module.exports = {
    screenTitle, checkConfig, chainHasTap, fill, sel, PRESETS, DEFAULT_PRESET, ICE_SLOTS,
    validateIcecast, validateLan, validateRecord, validateMonitor,
    applyIcecast, applyLan, applyRecord, applyMonitor, uplinksFrom, summarize
};

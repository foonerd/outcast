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
// no user, a stream number instead, and MP3 or AAC alone.
function protocolOf(data) {
    return sel(data.ice_protocol) === 'shoutcast1' ? 'shoutcast1' : 'icecast2';
}

function validateIcecast(data, t) {
    if (data.ice_enabled !== true) return null;
    const shoutcast = protocolOf(data) === 'shoutcast1';
    return (!String(data.ice_host || '').trim() ? t('ERR_HOST') : null) ||
        checkPort(data.ice_port, 'ICE_PORT', t) ||
        (shoutcast && parseInt(String(data.ice_port).trim(), 10) === 65535 ? t('ERR_PORT', { field: t('ICE_PORT') }) : null) ||
        (!shoutcast && !/^\/\S*$/.test(String(data.ice_mount || '').trim()) ? t('ERR_MOUNT') : null) ||
        (shoutcast ? checkRange(data.ice_stream_id, 'ICE_STREAM_ID', 1, 99, t) : null) ||
        (shoutcast && !['mp3', 'aac'].includes(sel(data.ice_codec)) ? t('ERR_SHOUTCAST_CODEC') : null) ||
        (!data.ice_password ? t('ERR_PASSWORD') : null) ||
        (!shoutcast ? checkText(data.ice_user, 'ICE_USER', t) : null) ||
        checkText(data.ice_name, 'STREAM_NAME', t) ||
        (data.ice_more === true && (
            checkText(data.ice_description, 'ICE_DESCRIPTION', t) ||
            checkText(data.ice_genre, 'ICE_GENRE', t) ||
            checkText(data.ice_url, 'ICE_URL', t))) ||
        null;
}

function validateLan(data, t) {
    if (data.lan_enabled !== true) return null;
    return checkPort(data.lan_port, 'LAN_PORT', t) ||
        checkText(data.lan_name, 'STREAM_NAME', t) ||
        checkRange(data.lan_max_listeners, 'LAN_MAX', 1, 64, t) ||
        null;
}

function validateRecord(data, t) {
    if (data.rec_enabled !== true) return null;
    const folder = String(data.rec_folder || '').trim();
    return (!/^\/\S/.test(folder) || /["\r\n]/.test(folder) ? t('ERR_FOLDER') : null) || null;
}

// ------------------------------------------------------------- the settings kept
// What a save writes into the plugin's configuration: the page's data as plain values.

function applyIcecast(data, set) {
    const on = data.ice_enabled === true;
    set('ice_enabled', on);
    if (!on) return;
    const shoutcast = protocolOf(data) === 'shoutcast1';
    set('ice_protocol', shoutcast ? 'shoutcast1' : 'icecast2');
    set('ice_host', String(data.ice_host).trim());
    set('ice_port', String(data.ice_port).trim());
    if (shoutcast) set('ice_stream_id', String(data.ice_stream_id).trim());
    else set('ice_mount', String(data.ice_mount).trim());
    if (!shoutcast) set('ice_user', String(data.ice_user || 'source').trim() || 'source');
    set('ice_password', String(data.ice_password));
    set('ice_codec', sel(data.ice_codec));
    set('ice_bitrate', sel(data.ice_bitrate));
    set('ice_rate', sel(data.ice_rate));
    set('ice_name', String(data.ice_name || '').trim());
    set('ice_keep_alive', data.ice_keep_alive !== false);
    set('ice_more', data.ice_more === true);
    if (data.ice_more === true) {
        set('ice_description', String(data.ice_description || '').trim());
        set('ice_genre', String(data.ice_genre || '').trim());
        set('ice_url', String(data.ice_url || '').trim());
        set('ice_public', data.ice_public === true);
    }
}

function applyLan(data, set) {
    const on = data.lan_enabled === true;
    set('lan_enabled', on);
    if (!on) return;
    set('lan_port', String(data.lan_port).trim());
    set('lan_codec', sel(data.lan_codec));
    set('lan_bitrate', sel(data.lan_bitrate));
    set('lan_rate', sel(data.lan_rate));
    set('lan_name', String(data.lan_name || '').trim());
    set('lan_max_listeners', String(data.lan_max_listeners).trim());
    set('lan_keep_alive', data.lan_keep_alive !== false);
}

function applyRecord(data, set) {
    const on = data.rec_enabled === true;
    set('rec_enabled', on);
    if (!on) return;
    set('rec_folder', String(data.rec_folder).trim().replace(/\/+$/, '') || '/');
    set('rec_codec', sel(data.rec_codec));
    set('rec_bitrate', sel(data.rec_bitrate));
    set('rec_rate', sel(data.rec_rate));
    set('rec_split', sel(data.rec_split));
    set('rec_title_in_name', data.rec_title_in_name !== false);
}

// The file the core reads, from the configuration: one object per uplink, in the
// core's own words (bins/outcast/src/config.rs).
function uplinksFrom(get) {
    const str = (k, d) => String(get(k, d));
    const bool = (k) => get(k, false) === true;
    return {
        icecast: {
            enabled: bool('ice_enabled'),
            protocol: str('ice_protocol', 'icecast2') === 'shoutcast1' ? 'shoutcast1' : 'icecast2',
            host: str('ice_host', ''),
            port: num(get('ice_port', '8000'), 8000),
            mount: str('ice_mount', '/volumio.mp3'),
            stream_id: num(get('ice_stream_id', '1'), 1),
            user: str('ice_user', 'source'),
            password: str('ice_password', ''),
            codec: str('ice_codec', 'mp3'),
            bitrate: num(get('ice_bitrate', '192'), 192),
            rate: num(get('ice_rate', '44100'), 44100),
            name: str('ice_name', 'Volumio'),
            description: bool('ice_more') ? str('ice_description', '') : '',
            genre: bool('ice_more') ? str('ice_genre', '') : '',
            url: bool('ice_more') ? str('ice_url', '') : '',
            public: bool('ice_more') && bool('ice_public'),
            keep_alive: get('ice_keep_alive', true) !== false
        },
        lan: {
            enabled: bool('lan_enabled'),
            port: num(get('lan_port', '8000'), 8000),
            codec: str('lan_codec', 'mp3'),
            bitrate: num(get('lan_bitrate', '192'), 192),
            rate: num(get('lan_rate', '44100'), 44100),
            name: str('lan_name', 'Volumio'),
            max_listeners: num(get('lan_max_listeners', '5'), 5),
            keep_alive: get('lan_keep_alive', true) !== false
        },
        record: {
            enabled: bool('rec_enabled'),
            folder: str('rec_folder', '/data/INTERNAL/outcast/recordings'),
            codec: str('rec_codec', 'flac'),
            bitrate: num(get('rec_bitrate', '192'), 192),
            rate: num(get('rec_rate', '0'), 0),
            split: str('rec_split', 'track'),
            title_in_name: get('rec_title_in_name', true) !== false
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
    if (audioPath === 'ready') lines.push(t('STATUS_TAP_READY'));
    else if (audioPath === 'lost') lines.push(t('STATUS_TAP_LOST'));
    else lines.push(t('STATUS_TAP_FAILED', { error: audioPathError || '' }));
    const names = { icecast: 'UPLINK_ICECAST', lan: 'UPLINK_LAN', record: 'UPLINK_RECORD' };
    if (!status || !Array.isArray(status.uplinks) || status.uplinks.length === 0) {
        lines.push(t('STATUS_NO_UPLINKS'));
    } else {
        status.uplinks.forEach((u) => {
            const name = names[u.kind] ? t(names[u.kind]) : u.id;
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
    screenTitle, checkConfig, chainHasTap, fill, sel,
    validateIcecast, validateLan, validateRecord,
    applyIcecast, applyLan, applyRecord, uplinksFrom, summarize
};

'use strict';
// The plugin's pure parts: the title as the screen shows it, the preflight
// configuration, the chain check, the validation and the settings, the file the core
// reads, the status summary.
const test = require('node:test');
const assert = require('node:assert/strict');
const h = require('../helpers');

const t = (k, v) => k + (v ? ' ' + JSON.stringify(v) : '');

test('the title is what the screen shows', () => {
    assert.equal(h.screenTitle({ artist: 'Artist', title: 'Title' }), 'Artist - Title');
    assert.equal(h.screenTitle({ artist: ' Artist ', title: '' }), 'Artist');
    assert.equal(h.screenTitle({ title: 'Only' }), 'Only');
    assert.equal(h.screenTitle({}), '');
    assert.equal(h.screenTitle(null), '');
});

test('the preflight configuration loads the library into a PCM writing into nothing', () => {
    const text = h.checkConfig('/data/plugins/audio_interface/outcast/lib/libevotap.so', 'outcast-check');
    assert.ok(text.startsWith('</usr/share/alsa/alsa.conf>\n'));
    assert.match(text, /pcm_type\.evotap \{\n  lib \/data\/plugins\/audio_interface\/outcast\/lib\/libevotap\.so\n\}/);
    assert.match(text, /pcm\.OutcastCheck \{\n  type evotap\n  slave\.pcm "null"\n  ring "outcast-check"\n  audio 1\n\}/);
    assert.ok(text.endsWith('\n'));
    assert.doesNotMatch(text, /postOutcast|volumio/);
});

test('the chain carries the tap when it defines pcm.Outcast', () => {
    assert.equal(h.chainHasTap('pcm.volumio {\n}\npcm.Outcast {\n  type evotap\n}\n'), true);
    assert.equal(h.chainHasTap('pcm.volumio {\n}\npcm.OutcastCheck {\n}\n'), false);
    assert.equal(h.chainHasTap('# pcm.Outcast {\n'), false);
    assert.equal(h.chainHasTap(''), false);
    assert.equal(h.chainHasTap(null), false);
});

test('a string is filled with its variables, and a select gives its value', () => {
    assert.equal(h.fill('{{a}} and {{b}} and {{a}}', { a: 1, b: 'two' }), '1 and two and 1');
    assert.equal(h.fill('plain', {}), 'plain');
    assert.equal(h.sel({ value: 'opus', label: 'Opus' }), 'opus');
    assert.equal(h.sel('mp3'), 'mp3');
    assert.equal(h.sel(undefined), '');
});

test('the Icecast settings are checked for what the server needs', () => {
    const good = { ice_enabled: true, ice_host: 'ice.example.com', ice_port: '8000', ice_mount: '/live.mp3', ice_password: 'x', ice_user: 'source', ice_name: 'Volumio' };
    assert.equal(h.validateIcecast(good, t), null);
    assert.equal(h.validateIcecast({ ice_enabled: false }, t), null);
    assert.equal(h.validateIcecast({ ...good, ice_host: ' ' }, t), 'ERR_HOST');
    assert.equal(h.validateIcecast({ ...good, ice_port: '70000' }, t), 'ERR_PORT {"field":"ICE_PORT"}');
    assert.equal(h.validateIcecast({ ...good, ice_mount: 'live' }, t), 'ERR_MOUNT');
    assert.equal(h.validateIcecast({ ...good, ice_mount: '/a b' }, t), 'ERR_MOUNT');
    assert.equal(h.validateIcecast({ ...good, ice_password: '' }, t), 'ERR_PASSWORD');
    assert.equal(h.validateIcecast({ ...good, ice_name: 'a"b' }, t), 'ERR_TEXT {"field":"STREAM_NAME"}');
    assert.equal(h.validateIcecast({ ...good, ice_more: true, ice_genre: 'a\nb' }, t), 'ERR_TEXT {"field":"ICE_GENRE"}');
});

test('a SHOUTcast server wants no mount, a stream number, and MP3 or AAC', () => {
    const sc = { ice_enabled: true, ice_protocol: { value: 'shoutcast1' }, ice_host: 'dnas.example.com', ice_port: '8000', ice_mount: 'nothing', ice_stream_id: '1', ice_password: 'x', ice_codec: { value: 'mp3' }, ice_name: 'Volumio' };
    assert.equal(h.validateIcecast(sc, t), null);
    assert.equal(h.validateIcecast({ ...sc, ice_codec: 'aac', ice_stream_id: '12' }, t), null);
    assert.equal(h.validateIcecast({ ...sc, ice_codec: { value: 'opus' } }, t), 'ERR_SHOUTCAST_CODEC');
    assert.equal(h.validateIcecast({ ...sc, ice_stream_id: '0' }, t), 'ERR_RANGE {"field":"ICE_STREAM_ID","min":1,"max":99}');
    assert.equal(h.validateIcecast({ ...sc, ice_stream_id: 'one' }, t), 'ERR_NUMBER {"field":"ICE_STREAM_ID"}');
    assert.equal(h.validateIcecast({ ...sc, ice_port: '65535' }, t), 'ERR_PORT {"field":"ICE_PORT"}');
    assert.equal(h.validateIcecast({ ...sc, ice_user: 'a"b' }, t), null);
    const kept = {};
    h.applyIcecast({ ...sc, ice_bitrate: '128', ice_rate: '44100', ice_stream_id: ' 2 ' }, (k, v) => { kept[k] = v; });
    assert.equal(kept.ice_protocol, 'shoutcast1');
    assert.equal(kept.ice_stream_id, '2');
    assert.equal('ice_mount' in kept, false);
    assert.equal('ice_user' in kept, false);
    const u = h.uplinksFrom((k, d) => (k in kept ? kept[k] : d));
    assert.equal(u.icecast.protocol, 'shoutcast1');
    assert.equal(u.icecast.stream_id, 2);
    assert.equal(u.icecast.mount, '/volumio.mp3');
    assert.equal(h.uplinksFrom((k, d) => d).icecast.protocol, 'icecast2');
});

test('the network stream and the recording are checked too', () => {
    assert.equal(h.validateLan({ lan_enabled: true, lan_port: '8000', lan_name: 'x', lan_max_listeners: '5' }, t), null);
    assert.equal(h.validateLan({ lan_enabled: true, lan_port: '0', lan_name: 'x', lan_max_listeners: '5' }, t), 'ERR_PORT {"field":"LAN_PORT"}');
    assert.equal(h.validateLan({ lan_enabled: true, lan_port: '8000', lan_name: 'x', lan_max_listeners: '99' }, t), 'ERR_RANGE {"field":"LAN_MAX","min":1,"max":64}');
    assert.equal(h.validateLan({ lan_enabled: true, lan_port: '8000', lan_name: 'x', lan_max_listeners: 'many' }, t), 'ERR_NUMBER {"field":"LAN_MAX"}');
    assert.equal(h.validateRecord({ rec_enabled: true, rec_folder: '/data/INTERNAL/outcast/recordings' }, t), null);
    assert.equal(h.validateRecord({ rec_enabled: true, rec_folder: 'recordings' }, t), 'ERR_FOLDER');
    assert.equal(h.validateRecord({ rec_enabled: true, rec_folder: '/' }, t), 'ERR_FOLDER');
    assert.equal(h.validateRecord({ rec_enabled: false, rec_folder: '' }, t), null);
});

test('a save keeps the page values, and the file the core reads follows', () => {
    const kept = {};
    const set = (k, v) => { kept[k] = v; };
    h.applyIcecast({ ice_enabled: true, ice_host: ' ice.example.com ', ice_port: '8000', ice_mount: '/live.opus', ice_user: '', ice_password: 'pw',
        ice_codec: { value: 'opus' }, ice_bitrate: { value: '128' }, ice_rate: { value: '48000' }, ice_name: 'Living room', ice_more: true,
        ice_description: 'd', ice_genre: 'g', ice_url: 'u', ice_public: true }, set);
    h.applyLan({ lan_enabled: true, lan_port: '8001', lan_codec: { value: 'flac' }, lan_bitrate: { value: '320' }, lan_rate: { value: '0' }, lan_name: 'LAN', lan_max_listeners: '3' }, set);
    h.applyRecord({ rec_enabled: true, rec_folder: '/data/INTERNAL/outcast/rec/', rec_codec: { value: 'wav' }, rec_bitrate: { value: '192' }, rec_rate: { value: '48000' }, rec_split: { value: 'stream' }, rec_title_in_name: false }, set);
    assert.equal(kept.ice_host, 'ice.example.com');
    assert.equal(kept.ice_user, 'source');
    assert.equal(kept.rec_folder, '/data/INTERNAL/outcast/rec');
    const get = (k, d) => (k in kept ? kept[k] : d);
    const u = h.uplinksFrom(get);
    assert.deepEqual(u.icecast, { enabled: true, protocol: 'icecast2', host: 'ice.example.com', port: 8000, mount: '/live.opus', stream_id: 1, user: 'source', password: 'pw', codec: 'opus', bitrate: 128, rate: 48000, name: 'Living room', description: 'd', genre: 'g', url: 'u', public: true, keep_alive: true });
    assert.deepEqual(u.lan, { enabled: true, port: 8001, codec: 'flac', bitrate: 320, rate: 0, name: 'LAN', max_listeners: 3, keep_alive: true });
    h.applyLan({ lan_enabled: true, lan_port: '8001', lan_codec: 'mp3', lan_bitrate: '192', lan_rate: '44100', lan_name: 'LAN', lan_max_listeners: '3', lan_keep_alive: false }, set);
    assert.equal(h.uplinksFrom(get).lan.keep_alive, false);
    assert.deepEqual(u.record, { enabled: true, folder: '/data/INTERNAL/outcast/rec', codec: 'wav', bitrate: 192, rate: 48000, split: 'stream', title_in_name: false });
    // Turned off, the extra fields are not carried.
    h.applyIcecast({ ice_enabled: true, ice_host: 'h', ice_port: '1', ice_mount: '/m', ice_password: 'p', ice_codec: 'mp3', ice_bitrate: '192', ice_rate: '44100', ice_name: 'n', ice_more: false }, set);
    assert.equal(h.uplinksFrom(get).icecast.description, '');
    assert.equal(h.uplinksFrom(get).icecast.public, false);
    const defaults = h.uplinksFrom((k, d) => d);
    assert.equal(defaults.icecast.enabled, false);
    assert.equal(defaults.icecast.keep_alive, true);
    assert.equal(defaults.lan.keep_alive, true);
    assert.equal(defaults.record.codec, 'flac');
    assert.equal(defaults.record.rate, 0);
});

test('the status summary says the core, the stream, the tap and each uplink', () => {
    assert.deepEqual(h.summarize(null, 'failed', 'boom', t),
        ['STATUS_CORE_DOWN', 'STATUS_TAP_FAILED {"error":"boom"}', 'STATUS_NO_UPLINKS']);
    const playing = { ring: { rate: 48000, channels: 2, format: 'S32_LE' }, segments: 3, bytes: 3 * 1048576, uplinks: [] };
    assert.deepEqual(h.summarize(playing, 'ready', '', t),
        ['STATUS_PLAYING {"rate":48000,"channels":2,"format":"S32_LE","segments":3,"mb":"3.0"}', 'STATUS_TAP_READY', 'STATUS_NO_UPLINKS']);
    const busy = { ring: null, segments: 0, bytes: 0, uplinks: [
        { id: 'icecast', kind: 'icecast', state: 'connected', detail: 'ice:8000/m', title: 'A - B', last_error: '', silence: true },
        { id: 'lan', kind: 'lan', state: 'listening', detail: 'port 8000', listeners: 2, last_error: '' },
        { id: 'record', kind: 'record', state: 'error', detail: '/x', last_error: 'no space', files: 0 }
    ] };
    assert.deepEqual(h.summarize(busy, 'lost', '', t), [
        'STATUS_IDLE', 'STATUS_TAP_LOST',
        'STATUS_UPLINK {"name":"UPLINK_ICECAST","state":"connected","detail":"ice:8000/m, silence, A - B"}',
        'STATUS_UPLINK {"name":"UPLINK_LAN","state":"listening","detail":"port 8000, 2 listening"}',
        'STATUS_UPLINK_ERROR {"name":"UPLINK_RECORD","state":"error","error":"no space"}'
    ]);
});

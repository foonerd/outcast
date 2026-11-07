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
    // The libraries the chain names, in order; the last serves.
    const chain = 'pcm_type.evotap {\n  lib /data/plugins/audio_interface/outcast/lib/libevotap.so\n}\npcm.Outcast {\n  type evotap\n}\npcm_type.evotap { lib "/data/plugins/user_interface/glass/lib/armv7/libevotap.so" }\n';
    assert.deepEqual(h.chainLibraries(chain), ['/data/plugins/audio_interface/outcast/lib/libevotap.so', '/data/plugins/user_interface/glass/lib/armv7/libevotap.so']);
    assert.deepEqual(h.chainLibraries('pcm.volumio {}'), []);
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

test('a SHOUTcast server wants no mount and a stream number; the codec is the operator\'s', () => {
    const sc = { ice_enabled: true, ice_protocol: { value: 'shoutcast1' }, ice_host: 'dnas.example.com', ice_port: '8000', ice_mount: 'nothing', ice_stream_id: '1', ice_password: 'x', ice_codec: { value: 'mp3' }, ice_name: 'Volumio' };
    assert.equal(h.validateIcecast(sc, t), null);
    assert.equal(h.validateIcecast({ ...sc, ice_codec: 'aac', ice_stream_id: '12' }, t), null);
    assert.equal(h.validateIcecast({ ...sc, ice_advanced: true, ice_codec: { value: 'opus' }, ice_bitrate: '64', ice_rate: '48000' }, t), null);
    assert.equal(h.validateIcecast({ ...sc, ice_stream_id: '0' }, t), 'ERR_RANGE {"field":"ICE_STREAM_ID","min":1,"max":999}');
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

test('the encoding is a preset, or the operator\'s own under Advanced, and nothing is refused', () => {
    // A preset: its values go to the core, the custom fields are not kept.
    const kept = {};
    const set = (k, v) => { kept[k] = v; };
    const base = { lan_enabled: true, lan_port: '8100', lan_name: 'x', lan_max_listeners: '5' };
    assert.equal(h.validateLan({ ...base, lan_preset: { value: 'opus-96-48000' } }, t), null);
    h.applyLan({ ...base, lan_preset: { value: 'opus-96-48000' }, lan_advanced: false, lan_codec: 'mp3', lan_bitrate: '999' }, set);
    assert.equal(kept.lan_preset, 'opus-96-48000');
    assert.equal('lan_bitrate' in kept, false);
    let u = h.uplinksFrom((k, d) => (k in kept ? kept[k] : d));
    assert.deepEqual([u.lan.codec, u.lan.bitrate, u.lan.rate, u.lan.channels], ['opus', 96, 48000, 2]);
    // Advanced: any number is taken as it is; only a non-number is refused.
    const adv = { ...base, lan_preset: 'mp3-128-44100', lan_advanced: true, lan_codec: { value: 'flac' }, lan_bitrate: '0', lan_rate: '96000', lan_channels: { value: '0' } };
    assert.equal(h.validateLan(adv, t), null);
    assert.equal(h.validateLan({ ...adv, lan_rate: 'cd' }, t), 'ERR_NUMBER {"field":"RATE"}');
    assert.equal(h.validateLan({ ...adv, lan_bitrate: '-1' }, t), 'ERR_NUMBER {"field":"BITRATE"}');
    assert.equal(h.validateLan({ ...adv, lan_bitrate: '5000', lan_rate: '192000' }, t), null);
    h.applyLan(adv, set);
    u = h.uplinksFrom((k, d) => (k in kept ? kept[k] : d));
    assert.deepEqual([u.lan.codec, u.lan.bitrate, u.lan.rate, u.lan.channels], ['flac', 0, 96000, 0]);
    // An unknown preset falls back to the section's default.
    h.applyLan({ ...base, lan_preset: 'nothing-such', lan_advanced: false }, set);
    assert.equal(kept.lan_preset, 'mp3-128-44100');
    // A player upgraded from a release without presets keeps the values it had.
    const old = { ice_enabled: true, ice_codec: 'aac', ice_bitrate: '160', ice_rate: '48000' };
    const v = h.uplinksFrom((k, d) => (k in old ? old[k] : d));
    assert.deepEqual([v.icecast.codec, v.icecast.bitrate, v.icecast.rate, v.icecast.channels], ['aac', 160, 48000, 2]);
    // The defaults: internet radio for the streams, FLAC as played for the files.
    const defaults = h.uplinksFrom((k, d) => d);
    assert.deepEqual([defaults.icecast.codec, defaults.icecast.bitrate, defaults.icecast.rate, defaults.icecast.channels], ['mp3', 128, 44100, 2]);
    assert.deepEqual([defaults.record.codec, defaults.record.bitrate, defaults.record.rate, defaults.record.channels], ['flac', 0, 0, 0]);
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
        ice_advanced: true, ice_codec: { value: 'opus' }, ice_bitrate: '128', ice_rate: '48000', ice_channels: { value: '2' }, ice_name: 'Living room', ice_more: true,
        ice_description: 'd', ice_genre: 'g', ice_url: 'u', ice_public: true }, set);
    h.applyLan({ lan_enabled: true, lan_port: '8001', lan_advanced: true, lan_codec: { value: 'flac' }, lan_bitrate: '320', lan_rate: '0', lan_channels: '0', lan_name: 'LAN', lan_max_listeners: '3' }, set);
    h.applyRecord({ rec_enabled: true, rec_folder: '/data/INTERNAL/outcast/rec/', rec_advanced: true, rec_codec: { value: 'wav' }, rec_bitrate: '192', rec_rate: '48000', rec_channels: '1', rec_split: { value: 'stream' }, rec_title_in_name: false }, set);
    assert.equal(kept.ice_host, 'ice.example.com');
    assert.equal(kept.ice_user, 'source');
    assert.equal(kept.rec_folder, '/data/INTERNAL/outcast/rec');
    const get = (k, d) => (k in kept ? kept[k] : d);
    const u = h.uplinksFrom(get);
    assert.deepEqual(u.icecast, { enabled: true, protocol: 'icecast2', host: 'ice.example.com', port: 8000, mount: '/live.opus', stream_id: 1, user: 'source', password: 'pw', codec: 'opus', bitrate: 128, rate: 48000, channels: 2, name: 'Living room', description: 'd', genre: 'g', url: 'u', public: true, keep_alive: true });
    assert.deepEqual(u.lan, { enabled: true, port: 8001, codec: 'flac', bitrate: 320, rate: 0, channels: 0, name: 'LAN', max_listeners: 3, keep_alive: true });
    h.applyLan({ lan_enabled: true, lan_port: '8001', lan_preset: 'mp3-192-44100', lan_name: 'LAN', lan_max_listeners: '3', lan_keep_alive: false }, set);
    assert.equal(h.uplinksFrom(get).lan.keep_alive, false);
    assert.equal(h.uplinksFrom(get).lan.bitrate, 192);
    assert.deepEqual(u.record, { enabled: true, folder: '/data/INTERNAL/outcast/rec', codec: 'wav', bitrate: 192, rate: 48000, channels: 1, split: 'stream', title_in_name: false });
    // Turned off, the extra fields are not carried.
    h.applyIcecast({ ice_enabled: true, ice_host: 'h', ice_port: '1', ice_mount: '/m', ice_password: 'p', ice_preset: 'mp3-128-44100', ice_name: 'n', ice_more: false }, set);
    assert.equal(h.uplinksFrom(get).icecast.description, '');
    assert.equal(h.uplinksFrom(get).icecast.public, false);
    const defaults = h.uplinksFrom((k, d) => d);
    assert.equal(defaults.icecast.enabled, false);
    assert.equal(defaults.icecast.keep_alive, true);
    assert.equal(defaults.lan.keep_alive, true);
    assert.equal(defaults.record.codec, 'flac');
    assert.equal(defaults.record.rate, 0);
});

test('the second and third Icecast slots are their own sections and their own uplinks', () => {
    assert.deepEqual(h.ICE_SLOTS, ['ice', 'ice2', 'ice3']);
    const good = { ice2_enabled: true, ice2_host: 'two.example.com', ice2_port: '8000', ice2_mount: '/two.mp3', ice2_password: 'x', ice2_user: 'source', ice2_name: 'Two' };
    assert.equal(h.validateIcecast(good, t, 'ice2'), null);
    assert.equal(h.validateIcecast({ ...good, ice2_mount: 'two' }, t, 'ice2'), 'ERR_MOUNT');
    assert.equal(h.validateIcecast({ ice2_enabled: false }, t, 'ice2'), null);
    const kept = {};
    h.applyIcecast(good, (k, v) => { kept[k] = v; }, 'ice2');
    assert.equal(kept.ice2_host, 'two.example.com');
    assert.equal('ice_host' in kept, false);
    const u = h.uplinksFrom((k, d) => (k in kept ? kept[k] : d));
    assert.equal(u.icecasts.length, 2);
    assert.equal(u.icecasts[0].enabled, true);
    assert.equal(u.icecasts[0].mount, '/two.mp3');
    assert.equal(u.icecasts[1].enabled, false);
    assert.equal(u.icecast.enabled, false);
    const s = { ring: null, segments: 0, bytes: 0, uplinks: [
        { id: 'icecast', kind: 'icecast', state: 'connected', detail: 'one', last_error: '' },
        { id: 'icecast2', kind: 'icecast', state: 'waiting', detail: 'two', last_error: 'refused' }
    ] };
    assert.deepEqual(h.summarize(s, 'ready', '', t).slice(2), [
        'STATUS_UPLINK {"name":"UPLINK_ICECAST","state":"connected","detail":"one"}',
        'STATUS_UPLINK_ERROR {"name":"UPLINK_ICECAST 2","state":"waiting","error":"refused"}'
    ]);
});

test('the SRT uplink takes a listener, a stream id, a passphrase and a latency', () => {
    const good = { srt_enabled: true, srt_host: 'switcher.example.com', srt_port: '9000', srt_stream_id: '', srt_passphrase: '', srt_latency: '120' };
    assert.equal(h.validateSrt(good, t), null);
    assert.equal(h.validateSrt({ srt_enabled: false }, t), null);
    assert.equal(h.validateSrt({ ...good, srt_host: '' }, t), 'ERR_HOST');
    assert.equal(h.validateSrt({ ...good, srt_port: '0' }, t), 'ERR_PORT {"field":"SRT_PORT"}');
    assert.equal(h.validateSrt({ ...good, srt_passphrase: 'short' }, t), 'ERR_PASSPHRASE');
    assert.equal(h.validateSrt({ ...good, srt_passphrase: 'long enough one' }, t), null);
    assert.equal(h.validateSrt({ ...good, srt_latency: 'soon' }, t), 'ERR_NUMBER {"field":"SRT_LATENCY"}');
    assert.equal(h.validateSrt({ ...good, srt_advanced: true, srt_codec: { value: 'mp2' }, srt_bitrate: '192', srt_rate: '48000' }, t), null);
    const kept = {};
    h.applySrt({ ...good, srt_stream_id: ' live/one ', srt_latency: ' 250 ', srt_preset: { value: 'mp2-192-48000' } }, (k, v) => { kept[k] = v; });
    const u = h.uplinksFrom((k, d) => (k in kept ? kept[k] : d)).srt;
    assert.deepEqual(u, { enabled: true, host: 'switcher.example.com', port: 9000, stream_id: 'live/one', passphrase: '', latency_ms: 250, codec: 'mp2', bitrate: 192, rate: 48000, channels: 2, keep_alive: true });
    const defaults = h.uplinksFrom((k, d) => d).srt;
    assert.deepEqual([defaults.enabled, defaults.port, defaults.latency_ms, defaults.codec, defaults.rate], [false, 9000, 120, 'aac', 48000]);
    const s = { ring: null, segments: 0, bytes: 0, uplinks: [{ id: 'srt', kind: 'srt', state: 'connected', detail: 'srt://x:9000', last_error: '', rtt_ms: 3, lost_packets: 0 }] };
    const line = h.summarize(s, 'ready', '', t)[2];
    assert.ok(line.startsWith('STATUS_UPLINK {"name":"UPLINK_SRT","state":"connected","detail":"srt://x:9000, STATUS_SRT_LINK '), line);
    assert.ok(line.includes('rtt') && line.includes(':3') && line.includes('lost'), line);
});

test('the RTMP uplink takes an rtmp address with an application and a stream key', () => {
    const good = { rtmp_enabled: true, rtmp_url: 'rtmp://ingest.example.com/live', rtmp_key: 'abcd-1234' };
    assert.equal(h.validateRtmp(good, t), null);
    assert.equal(h.validateRtmp({ rtmp_enabled: false }, t), null);
    assert.equal(h.validateRtmp({ ...good, rtmp_url: 'rtmps://live-api-s.facebook.com:443/rtmp' }, t), null);
    assert.equal(h.validateRtmp({ ...good, rtmp_url: 'http://x/live' }, t), 'ERR_RTMP_URL');
    assert.equal(h.validateRtmp({ ...good, rtmp_url: 'rtmp://host' }, t), 'ERR_RTMP_URL');
    assert.equal(h.validateRtmp({ ...good, rtmp_key: '' }, t), 'ERR_RTMP_KEY');
    const kept = {};
    h.applyRtmp({ ...good, rtmp_url: ' rtmp://x:1936/app ', rtmp_preset: { value: 'aac-160-44100' } }, (k, v) => { kept[k] = v; });
    const u = h.uplinksFrom((k, d) => (k in kept ? kept[k] : d)).rtmp;
    assert.deepEqual(u, { enabled: true, url: 'rtmp://x:1936/app', stream_key: 'abcd-1234', ca_file: '', codec: 'aac', bitrate: 160, rate: 44100, channels: 2, keep_alive: true });
    // A private authority's file is an Advanced setting, kept only when Advanced is on.
    h.applyRtmp({ ...good, rtmp_ca_file: ' /data/ca.pem ' }, (k, v) => { kept[k] = v; });
    assert.equal(h.uplinksFrom((k, d) => (k in kept ? kept[k] : d)).rtmp.ca_file, '');
    h.applyRtmp({ ...good, rtmp_advanced: true, rtmp_codec: { value: 'aac' }, rtmp_bitrate: '128', rtmp_rate: '48000', rtmp_channels: { value: '2' }, rtmp_ca_file: ' /data/ca.pem ' }, (k, v) => { kept[k] = v; });
    assert.equal(h.uplinksFrom((k, d) => (k in kept ? kept[k] : d)).rtmp.ca_file, '/data/ca.pem');
    const content = [{ id: 'rtmp_enabled' }, { id: 'rtmp_codec' }, { id: 'rtmp_ca_file' }];
    h.hideWhenOff(content, 'rtmp', (k, d) => ({ rtmp_enabled: false, rtmp_advanced: true }[k] ?? d));
    assert.deepEqual(content.map((c) => c.hidden === true), [false, true, true]);
});

test('the transport-stream uplink goes over TCP unless told UDP', () => {
    const good = { ts_enabled: true, ts_host: 'decoder.example.com', ts_port: '5004', ts_ttl: '1' };
    assert.equal(h.validateTs(good, t), null);
    assert.equal(h.validateTs({ ...good, ts_port: '0' }, t), 'ERR_PORT {"field":"TS_PORT"}');
    assert.equal(h.validateTs({ ...good, ts_ttl: '300' }, t), 'ERR_RANGE {"field":"TS_TTL","min":1,"max":255}');
    const kept = {};
    h.applyTs(good, (k, v) => { kept[k] = v; });
    assert.equal(h.uplinksFrom((k, d) => (k in kept ? kept[k] : d)).ts.transport, 'tcp');
    h.applyTs({ ...good, ts_transport: { value: 'udp' }, ts_host: '239.1.1.1', ts_ttl: '4', ts_preset: { value: 'mp2-192-48000' } }, (k, v) => { kept[k] = v; });
    const u = h.uplinksFrom((k, d) => (k in kept ? kept[k] : d)).ts;
    assert.deepEqual(u, { enabled: true, transport: 'udp', host: '239.1.1.1', port: 5004, ttl: 4, codec: 'mp2', bitrate: 192, rate: 48000, channels: 2, keep_alive: true });
    assert.equal(h.uplinksFrom((k, d) => d).ts.transport, 'tcp');
});

test('the HLS uplink serves on a port, goes to an origin, or both, never nowhere', () => {
    const good = { hls_enabled: true, hls_name: 'live', hls_serve: true, hls_port: '8010', hls_max_connections: '64', hls_origin: { value: 'none' } };
    assert.equal(h.validateHls(good, t), null);
    assert.equal(h.validateHls({ hls_enabled: false }, t), null);
    assert.equal(h.validateHls({ ...good, hls_serve: false }, t), 'ERR_HLS_NOWHERE');
    assert.equal(h.validateHls({ ...good, hls_port: 'x' }, t), 'ERR_PORT {"field":"HLS_PORT"}');
    assert.equal(h.validateHls({ ...good, hls_max_connections: '0' }, t), 'ERR_RANGE {"field":"HLS_MAX_CONNECTIONS","min":1,"max":1000}');
    assert.equal(h.validateHls({ ...good, hls_origin: { value: 'http' }, hls_origin_url: 'ftp://x/y' }, t), 'ERR_HLS_ORIGIN_URL');
    assert.equal(h.validateHls({ ...good, hls_serve: false, hls_origin: { value: 'http' }, hls_origin_url: 'https://o.example.com/radio/' }, t), null);
    assert.equal(h.validateHls({ ...good, hls_advanced: true, hls_codec: { value: 'aac' }, hls_bitrate: '128', hls_rate: '48000', hls_segment_seconds: '0', hls_window: '6' }, t), 'ERR_RANGE {"field":"HLS_SEGMENT","min":1,"max":30}');
    const kept = {};
    h.applyHls({ ...good, hls_name: ' my station ', hls_origin: { value: 'http' }, hls_origin_url: ' http://o.example.com/radio ', hls_origin_user: 'ingest', hls_origin_password: 'pw' }, (k, v) => { kept[k] = v; });
    let u = h.uplinksFrom((k, d) => (k in kept ? kept[k] : d)).hls;
    assert.deepEqual(u, { enabled: true, name: 'my station', codec: 'aac', bitrate: 128, rate: 48000, channels: 2, segment_seconds: 4, window: 6, form: 'packed', dash: false, port: 8010, max_connections: 64, keep_alive: true, origin: 'http', origin_url: 'http://o.example.com/radio', origin_user: 'ingest', origin_password: 'pw', ca_file: '',
        s3_endpoint: '', s3_bucket: '', s3_prefix: '', s3_region: 'us-east-1', s3_access_key: '', s3_secret_key: '', s3_path_style: true });
    // A bucket: the endpoint, the bucket name, the keys are needed; the prefix is bare of slashes.
    const s3 = { ...good, hls_origin: { value: 's3' }, hls_s3_endpoint: 'https://s3.eu-west-1.amazonaws.com', hls_s3_bucket: 'my-radio', hls_s3_access_key: 'AKIA', hls_s3_secret_key: 'sk' };
    assert.equal(h.validateHls(s3, t), null);
    assert.equal(h.validateHls({ ...s3, hls_s3_endpoint: 's3.amazonaws.com' }, t), 'ERR_HLS_S3_ENDPOINT');
    assert.equal(h.validateHls({ ...s3, hls_s3_bucket: 'My Radio' }, t), 'ERR_HLS_S3_BUCKET');
    assert.equal(h.validateHls({ ...s3, hls_s3_secret_key: '' }, t), 'ERR_HLS_S3_KEYS');
    h.applyHls({ ...s3, hls_s3_prefix: '/station/live/', hls_s3_region: ' ', hls_s3_path_style: false, hls_s3_ca_file: '/data/ca.pem' }, (k, v) => { kept[k] = v; });
    u = h.uplinksFrom((k, d) => (k in kept ? kept[k] : d)).hls;
    assert.deepEqual([u.origin, u.s3_endpoint, u.s3_bucket, u.s3_prefix, u.s3_region, u.s3_access_key, u.s3_secret_key, u.s3_path_style, u.ca_file],
        ['s3', 'https://s3.eu-west-1.amazonaws.com', 'my-radio', 'station/live', 'us-east-1', 'AKIA', 'sk', false, '/data/ca.pem']);
    // Not served on the player: the port is 0 for the core; the segment length and the
    // window are Advanced settings.
    h.applyHls({ ...good, hls_serve: false, hls_origin: { value: 'http' }, hls_origin_url: 'https://o.example.com/', hls_advanced: true, hls_codec: { value: 'mp3' }, hls_bitrate: '192', hls_rate: '44100', hls_channels: { value: '2' }, hls_segment_seconds: '6', hls_window: '10' }, (k, v) => { kept[k] = v; });
    u = h.uplinksFrom((k, d) => (k in kept ? kept[k] : d)).hls;
    assert.equal(u.port, 0);
    assert.deepEqual([u.codec, u.bitrate, u.rate, u.segment_seconds, u.window], ['mp3', 192, 44100, 6, 10]);
    assert.equal(h.uplinksFrom((k, d) => d).hls.origin, 'none');
    // Packed audio takes AAC or MP3 alone; fragmented MP4 takes FLAC and Opus, and DASH rides on it.
    assert.equal(h.validateHls({ ...good, hls_preset: { value: 'flac-48000' } }, t), 'ERR_HLS_PACKED_CODEC');
    assert.equal(h.validateHls({ ...good, hls_preset: { value: 'flac-48000' }, hls_form: { value: 'fmp4' } }, t), null);
    assert.equal(h.validateHls({ ...good, hls_advanced: true, hls_codec: { value: 'opus' }, hls_bitrate: '96', hls_rate: '48000', hls_segment_seconds: '4', hls_window: '6' }, t), 'ERR_HLS_PACKED_CODEC');
    h.applyHls({ ...good, hls_form: { value: 'fmp4' }, hls_dash: true, hls_preset: { value: 'flac-48000' } }, (k, v) => { kept[k] = v; });
    u = h.uplinksFrom((k, d) => (k in kept ? kept[k] : d)).hls;
    assert.deepEqual([u.form, u.dash, u.codec, u.bitrate, u.rate], ['fmp4', true, 'flac', 0, 48000]);
    h.applyHls({ ...good, hls_form: { value: 'packed' }, hls_dash: true }, (k, v) => { kept[k] = v; });
    u = h.uplinksFrom((k, d) => (k in kept ? kept[k] : d)).hls;
    assert.deepEqual([u.form, u.dash], ['packed', false], 'DASH only with fragmented MP4');
    const content = [{ id: 'hls_enabled' }, { id: 'hls_port' }, { id: 'hls_window' }];
    h.hideWhenOff(content, 'hls', (k, d) => ({ hls_enabled: false, hls_serve: true, hls_advanced: true }[k] ?? d));
    assert.deepEqual(content.map((c) => c.hidden === true), [false, true, true]);
});

test('the dead-air watch takes a level and a time, and the file carries them', () => {
    assert.equal(h.validateMonitor({ mon_level: '-60', mon_seconds: '30' }, t), null);
    assert.equal(h.validateMonitor({ mon_level: '-45.5', mon_seconds: '0' }, t), null);
    assert.equal(h.validateMonitor({ mon_level: 'quiet', mon_seconds: '30' }, t), 'ERR_NUMBER {"field":"MON_LEVEL"}');
    assert.equal(h.validateMonitor({ mon_level: '-60', mon_seconds: '-1' }, t), 'ERR_NUMBER {"field":"MON_SECONDS"}');
    const kept = {};
    h.applyMonitor({ mon_level: ' -50 ', mon_seconds: ' 12 ' }, (k, v) => { kept[k] = v; });
    assert.deepEqual(h.uplinksFrom((k, d) => (k in kept ? kept[k] : d)).monitor, { dead_air_level: -50, dead_air_seconds: 12 });
    assert.deepEqual(h.uplinksFrom((k, d) => d).monitor, { dead_air_level: -60, dead_air_seconds: 30 });
    // Switched off: nothing checked, nothing kept but the switch, no dead air for the core.
    assert.equal(h.validateMonitor({ mon_enabled: false, mon_level: 'x' }, t), null);
    const off = {};
    h.applyMonitor({ mon_enabled: false, mon_level: '-10' }, (k, v) => { off[k] = v; });
    assert.deepEqual(off, { mon_enabled: false });
    assert.equal(h.uplinksFrom((k, d) => (k in off ? off[k] : d)).monitor.dead_air_seconds, 0);
});

test('a section switched off shows its switch alone, whatever its nested switches say', () => {
    const content = ['enabled', 'advanced', 'codec', 'bitrate', 'more', 'genre', 'name'].map((k) => ({ id: 'ice_' + k }));
    const cfg = { ice_enabled: false, ice_advanced: true, ice_more: true };
    h.hideWhenOff(content, 'ice', (k, d) => (k in cfg ? cfg[k] : d));
    assert.deepEqual(content.filter((c) => c.hidden).map((c) => c.id), ['ice_codec', 'ice_bitrate', 'ice_genre']);
    const on = ['enabled', 'advanced', 'codec'].map((k) => ({ id: 'lan_' + k }));
    h.hideWhenOff(on, 'lan', (k, d) => ({ lan_enabled: true, lan_advanced: true }[k] ?? d));
    assert.equal(on.some((c) => c.hidden), false);
});

test('the status summary says the level and dead air', () => {
    const s = { ring: null, segments: 0, bytes: 0, uplinks: [], level: { peak_db: [-6.02, -8.1], rms_db: [-20.5, -21.0], at: 1 }, dead_air: true, quiet_seconds: 42 };
    assert.deepEqual(h.summarize(s, 'ready', '', t), [
        'STATUS_IDLE',
        'STATUS_LEVEL {"peak":"-6.0","rms":"-20.5"}',
        'STATUS_DEAD_AIR {"seconds":42}',
        'STATUS_TAP_READY', 'STATUS_NO_UPLINKS'
    ]);
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

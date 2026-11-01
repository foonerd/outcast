'use strict';
// The plugin's pure parts: the title as the screen shows it, the preflight
// configuration, the chain check, the status summary.
const test = require('node:test');
const assert = require('node:assert/strict');
const { screenTitle, checkConfig, chainHasTap, summarize, fill } = require('../helpers');

test('the title is what the screen shows', () => {
    assert.equal(screenTitle({ artist: 'Artist', title: 'Title' }), 'Artist - Title');
    assert.equal(screenTitle({ artist: ' Artist ', title: '' }), 'Artist');
    assert.equal(screenTitle({ title: 'Only' }), 'Only');
    assert.equal(screenTitle({}), '');
    assert.equal(screenTitle(null), '');
});

test('the preflight configuration loads the library into a PCM writing into nothing', () => {
    const text = checkConfig('/data/plugins/audio_interface/outcast/lib/libevotap.so', 'outcast-check');
    assert.ok(text.startsWith('</usr/share/alsa/alsa.conf>\n'));
    assert.match(text, /pcm_type\.evotap \{\n  lib \/data\/plugins\/audio_interface\/outcast\/lib\/libevotap\.so\n\}/);
    assert.match(text, /pcm\.OutcastCheck \{\n  type evotap\n  slave\.pcm "null"\n  ring "outcast-check"\n  audio 1\n\}/);
    assert.ok(text.endsWith('\n'));
    assert.doesNotMatch(text, /postOutcast|volumio/);
});

test('the chain carries the tap when it defines pcm.Outcast', () => {
    assert.equal(chainHasTap('pcm.volumio {\n}\npcm.Outcast {\n  type evotap\n}\n'), true);
    assert.equal(chainHasTap('pcm.volumio {\n}\npcm.OutcastCheck {\n}\n'), false);
    assert.equal(chainHasTap('# pcm.Outcast {\n'), false);
    assert.equal(chainHasTap(''), false);
    assert.equal(chainHasTap(null), false);
});

test('the status summary says the core, the stream, the tap and the uplinks', () => {
    const t = (k, v) => k + (v ? ' ' + JSON.stringify(v) : '');
    assert.deepEqual(summarize(null, 'failed', 'boom', t),
        ['STATUS_CORE_DOWN', 'STATUS_TAP_FAILED {"error":"boom"}', 'STATUS_NO_UPLINKS']);
    const playing = { ring: { rate: 48000, channels: 2, format: 'S32_LE' }, segments: 3, bytes: 3 * 1048576, uplinks: [] };
    assert.deepEqual(summarize(playing, 'ready', '', t),
        ['STATUS_PLAYING {"rate":48000,"channels":2,"format":"S32_LE","segments":3,"mb":"3.0"}', 'STATUS_TAP_READY', 'STATUS_NO_UPLINKS']);
    const idle = { ring: null, segments: 0, bytes: 0, uplinks: [{ id: 'icecast-1', state: 'connected' }] };
    assert.deepEqual(summarize(idle, 'lost', '', t), ['STATUS_IDLE', 'STATUS_TAP_LOST', 'icecast-1: connected']);
});

test('a string is filled with its variables', () => {
    assert.equal(fill('{{a}} and {{b}} and {{a}}', { a: 1, b: 'two' }), '1 and two and 1');
    assert.equal(fill('plain', {}), 'plain');
    assert.equal(fill('{{missing}}', {}), '{{missing}}');
});

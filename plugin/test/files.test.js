'use strict';
// What the store and the player read: every file parses, every key resolves, every
// language carries every string, the scripts end with the store's words, the ALSA
// template and the unit name the plugin's own paths.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');

const root = path.join(__dirname, '..');
const read = (f) => fs.readFileSync(path.join(root, f), 'utf8');
const json = (f) => JSON.parse(read(f));
const PLUGIN_DIR = '/data/plugins/audio_interface/outcast';

test('package.json says what the store checks', () => {
    const p = json('package.json');
    assert.equal(p.name, 'outcast');
    assert.match(p.version, /^\d+\.\d+\.\d+$/);
    for (const k of ['description', 'author', 'license', 'repository', 'main']) assert.ok(p[k], k);
    assert.equal(p.main, 'index.js');
    const v = p.volumio_info;
    assert.equal(v.prettyName, 'Outcast Uplink');
    assert.equal(v.plugin_type, 'audio_interface');
    assert.equal(v.has_alsa_contribution, true);
    assert.deepEqual(v.os, ['bookworm']);
    for (const a of v.architectures) assert.ok(['amd64', 'armhf', 'i386'].includes(a), a);
    assert.ok(v.details && v.details.length < 400, 'details short');
    assert.ok(v.changelog && v.changelog.length < 400, 'changelog short');
    assert.equal(p.engines.node, '>=20');
    assert.equal(p.engines.volumio, '>=4');
    assert.ok(p.dependencies.kew && p.dependencies['v-conf']);
});

test('every TRANSLATE key of the page is in the English strings', () => {
    const ui = json('UIConfig.json');
    const en = json('i18n/strings_en.json').OUTCAST;
    const walk = (o, where) => {
        if (typeof o === 'string') {
            if (o.startsWith('TRANSLATE.')) {
                const k = o.slice('TRANSLATE.'.length);
                assert.ok(k.startsWith('OUTCAST.') && k.slice('OUTCAST.'.length) in en, where + ': ' + o);
            }
        } else if (o && typeof o === 'object') {
            Object.entries(o).forEach(([k, v]) => walk(v, where + '.' + k));
        }
    };
    walk(ui, 'UIConfig');
    for (const s of ui.sections) {
        for (const id of (s.saveButton && s.saveButton.data) || []) {
            assert.ok(s.content.find((c) => c.id === id), `section ${s.id}: saveButton names missing field ${id}`);
        }
        assert.equal(s.onSave.endpoint, 'audio_interface/outcast');
    }
});

test('the strings index.js uses exist, and every language carries every string', () => {
    const en = json('i18n/strings_en.json').OUTCAST;
    const idx = read('index.js');
    for (const m of idx.matchAll(/(?:i18n|toast)\((?:'[a-z]+',\s*)?'([A-Z_]+)'/g)) {
        assert.ok(m[1] in en, 'index.js uses missing string ' + m[1]);
    }
    for (const m of read('helpers.js').matchAll(/\bt\('([A-Z_]+)'/g)) {
        assert.ok(m[1] in en, 'helpers.js uses missing string ' + m[1]);
    }
    const placeholders = (s) => (String(s).match(/{{\w+}}/g) || []).sort().join(',');
    for (const f of fs.readdirSync(path.join(root, 'i18n'))) {
        const b = json('i18n/' + f).OUTCAST;
        for (const k of Object.keys(en)) {
            assert.ok(k in b, `${f}: missing ${k}`);
            assert.equal(placeholders(b[k]), placeholders(en[k]), `${f}: placeholders differ in ${k}`);
        }
        for (const k of Object.keys(b)) assert.ok(k in en, `${f}: extra key ${k}`);
    }
});

test('the install scripts end with the words the store looks for', () => {
    const install = read('install.sh').trimEnd().split('\n');
    const uninstall = read('uninstall.sh').trimEnd().split('\n');
    assert.equal(install[install.length - 1], 'echo "plugininstallend"');
    assert.equal(uninstall[uninstall.length - 1], 'echo "pluginuninstallend"');
    assert.ok(install[0].startsWith('#!/bin/sh'));
    assert.ok(uninstall[0].startsWith('#!/bin/sh'));
});

test('the ALSA template holds no variable and names the plugin\'s own library and ring', () => {
    const tmpl = read('Outcast.postOutcast.5.conf.tmpl');
    assert.doesNotMatch(tmpl, /\$\{/);
    assert.match(tmpl, /^pcm\.Outcast\s*\{/m);
    assert.match(tmpl, /slave\.pcm "postOutcast"/);
    assert.match(tmpl, /ring "outcast"/);
    assert.match(tmpl, /audio 1/);
    assert.match(tmpl, new RegExp('lib ' + PLUGIN_DIR.replace(/\//g, '\\/') + '/lib/libevotap\\.so'));
});

test('the unit runs the core as the volumio user from the plugin folder', () => {
    const unit = read('outcast.service');
    assert.match(unit, /^User=volumio$/m);
    assert.match(unit, new RegExp('^ExecStart=' + PLUGIN_DIR.replace(/\//g, '\\/') + '/bin/outcast --run /run/outcast$', 'm'));
    assert.match(unit, /^RuntimeDirectory=outcast$/m);
    assert.match(unit, /^Restart=always$/m);
    assert.match(unit, /^ReadWritePaths=.*\/data\/INTERNAL\/outcast/m);
});

test('every file the plugin ships ends with one newline', () => {
    for (const f of ['package.json', 'config.json', 'UIConfig.json', 'index.js', 'helpers.js', 'install.sh', 'uninstall.sh', 'outcast.service', 'Outcast.postOutcast.5.conf.tmpl']) {
        const text = read(f);
        assert.ok(text.endsWith('\n') && !text.endsWith('\n\n'), f + ': one final newline');
    }
    for (const f of fs.readdirSync(path.join(root, 'i18n'))) {
        assert.ok(read('i18n/' + f).endsWith('\n'), f + ': final newline');
    }
});

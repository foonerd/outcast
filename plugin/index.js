'use strict';
// Outcast Uplink: the plugin shell. It owns the audio path (the tap's ALSA contribution,
// proved with the player's own ALSA before it is registered and taken back when the
// chain does not take it), the core's service, the title from the player's state, and
// the settings page, whose settings it hands to the core as one file. The core
// (bin/outcast) carries the sound to the uplinks.

const libQ = require('kew');
const fs = require('fs');
const net = require('net');
const path = require('path');
const { execFile } = require('child_process');
const VConf = require('v-conf');
const h = require('./helpers');

const LOG = 'outcast: ';
const CONTRIBUTION = 'Outcast.postOutcast.5.conf';
const TEMPLATE = path.join(__dirname, CONTRIBUTION + '.tmpl');
const CONTRIBUTION_FILE = path.join(__dirname, 'asound', CONTRIBUTION);
const LIBRARY = path.join(__dirname, 'lib', 'libevotap.so');
const ALSA_FILE = '/etc/asound.conf';
const SERVICE = 'outcast.service';
const RUN_DIR = '/run/outcast';
const STATUS_FILE = RUN_DIR + '/status.json';
const CONTROL_SOCKET = RUN_DIR + '/control.sock';
const CHECK_CONF = '/tmp/outcast-check.conf';
const CHECK_TAG = 'outcast-check';

const msg = (e) => (e && e.message ? e.message : String(e));
// An error that names the string the user sees.
function keyed(key, e) {
    const err = new Error(msg(e));
    err.key = key;
    return err;
}

function Outcast(context) {
    this.context = context;
    this.commandRouter = context.coreCommand;
    this.logger = this.commandRouter.logger;
    this.config = new VConf();
    this.active = false;
    this.stateCallback = null;
    this.alsaWatcher = null;
    // off, ready, failed or lost: where the tap stands in the audio chain.
    this.audioPath = 'off';
    this.audioPathError = '';
    this.lastLoggedTitle = '';
    this.uplinksFile = null;
}

// ----------------------------------------------------------------- lifecycle

Outcast.prototype.onVolumioStart = function () {
    this.configFile = this.commandRouter.pluginManager.getConfigurationFile(this.context, 'config.json');
    this.config.loadFile(this.configFile);
    // The uplinks, as the core reads them, beside the configuration.
    this.uplinksFile = path.join(path.dirname(this.configFile), 'uplinks.json');
    return libQ.resolve();
};

Outcast.prototype.onStart = function () {
    const self = this;
    self.active = true;
    self.commandRouter.loadI18nStrings();
    self.subscribeState();
    // The core reads the uplinks at its start: the file is written first, whatever the
    // audio path's fate; the core's status says what it sees.
    try { self.writeUplinks(); } catch (e) { self.logger.error(LOG + 'uplinks: ' + msg(e)); }
    return self.installAudioPath()
        .fail(() => undefined)
        .then(() => self.startCore())
        .fail((e) => {
            self.logger.error(LOG + 'start: ' + msg(e));
            self.toast('error', 'ERR_CORE', { error: msg(e) });
        })
        .then(() => 'OK');
};

Outcast.prototype.onStop = function () {
    const self = this;
    self.active = false;
    self.unsubscribeState();
    self.unwatchChain();
    return self.stopCore()
        .fail((e) => { self.logger.warn(LOG + 'stop: ' + msg(e)); })
        .then(() => self.removeAudioPath())
        .fail((e) => { self.logger.error(LOG + 'stop: ' + msg(e)); })
        .then(() => 'OK');
};

Outcast.prototype.onRestart = function () {
    return libQ.resolve();
};

Outcast.prototype.getConfigurationFiles = function () {
    return ['config.json'];
};

Outcast.prototype.getI18nFile = function (langCode) {
    const file = path.join(__dirname, 'i18n', 'strings_' + langCode + '.json');
    return fs.existsSync(file) ? file : path.join(__dirname, 'i18n', 'strings_en.json');
};

// ---------------------------------------------------------------- audio path

// The tap into the player's chain, in four steps, none of which touches the chain
// before the one before it has passed: the library is there; the player's own ALSA
// loads it and plays a second through it into nothing; the contribution is written and
// the player asked to write its chain; the chain as written carries the tap and parses.
// A step that fails takes the contribution back, so nothing of the plugin stays in the
// chain, and says why.
Outcast.prototype.installAudioPath = function () {
    const self = this;
    const start = fs.existsSync(LIBRARY) ? libQ.resolve() : libQ.reject(keyed('ERR_LIBRARY', new Error(LIBRARY + ' is missing')));
    return start
        .then(() => self.preflight())
        .then(() => self.writeContribution())
        .then(() => self.updateChain())
        .then(() => self.verifyChain())
        .then(() => {
            self.audioPath = 'ready';
            self.audioPathError = '';
            self.watchChain();
            self.logger.info(LOG + 'the tap is in the audio chain');
        })
        .fail((e) => {
            const reason = msg(e);
            self.logger.error(LOG + 'audio path: ' + reason);
            self.audioPath = 'failed';
            self.audioPathError = reason;
            return self.removeAudioPath()
                .fail((back) => { self.logger.error(LOG + 'audio path: the back-out failed too: ' + msg(back)); })
                .then(() => {
                    self.toast('error', e && e.key ? e.key : 'ERR_CHAIN', { error: reason });
                    throw e;
                });
        });
};

// The library loaded by the player's own ALSA, as a PCM of its own that plays one
// second of silence into nothing: proves the library is this machine's, that ALSA takes
// the plugin type, and that the ring is made. The player's chain is not in it.
Outcast.prototype.preflight = function () {
    const self = this;
    const defer = libQ.defer();
    try {
        fs.writeFileSync(CHECK_CONF, h.checkConfig(LIBRARY, CHECK_TAG));
    } catch (e) {
        return libQ.reject(keyed('ERR_PREFLIGHT', e));
    }
    execFile('/usr/bin/aplay', ['-D', 'OutcastCheck', '-q', '-f', 'S16_LE', '-r', '48000', '-c', '2', '-d', '1', '/dev/zero'],
        { env: Object.assign({}, process.env, { ALSA_CONFIG_PATH: CHECK_CONF }), timeout: 15000 },
        (error, stdout, stderr) => {
            try { fs.unlinkSync(CHECK_CONF); } catch (e) { /* already gone */ }
            if (error) {
                defer.reject(keyed('ERR_PREFLIGHT', new Error(String(stderr || error.message).trim())));
            } else {
                self.logger.info(LOG + 'preflight: the tap loads and plays through');
                defer.resolve();
            }
        });
    return defer.promise;
};

Outcast.prototype.writeContribution = function () {
    try {
        fs.mkdirSync(path.dirname(CONTRIBUTION_FILE), { recursive: true });
        fs.writeFileSync(CONTRIBUTION_FILE, fs.readFileSync(TEMPLATE, 'utf8'), 'utf8');
        return libQ.resolve();
    } catch (e) {
        return libQ.reject(keyed('ERR_CHAIN', e));
    }
};

// The player writes its chain from every enabled plugin's contribution.
Outcast.prototype.updateChain = function () {
    const self = this;
    const defer = libQ.defer();
    let done = false;
    const finish = () => { if (!done) { done = true; defer.resolve(); } };
    let ret;
    try {
        ret = self.commandRouter.executeOnPlugin('audio_interface', 'alsa_controller', 'updateALSAConfigFile');
    } catch (e) {
        return libQ.reject(keyed('ERR_CHAIN', e));
    }
    if (ret && typeof ret.then === 'function') {
        ret.then(finish, (e) => { if (!done) { done = true; defer.reject(keyed('ERR_CHAIN', e)); } });
    } else {
        finish();
    }
    return defer.promise;
};

// The chain as written carries the tap, and the player's ALSA reads it whole.
Outcast.prototype.verifyChain = function () {
    const defer = libQ.defer();
    let text = '';
    try { text = fs.readFileSync(ALSA_FILE, 'utf8'); } catch (e) { return libQ.reject(keyed('ERR_CHAIN', e)); }
    if (!h.chainHasTap(text)) {
        return libQ.reject(keyed('ERR_CHAIN', new Error('the player did not take the contribution into ' + ALSA_FILE)));
    }
    execFile('/usr/bin/aplay', ['-L'], { timeout: 15000 }, (error, stdout, stderr) => {
        if (error) defer.reject(keyed('ERR_CHAIN', new Error(String(stderr || error.message).trim())));
        else defer.resolve();
    });
    return defer.promise;
};

// Nothing of the plugin left in the chain: the contribution gone, the chain written
// again where it was there.
Outcast.prototype.removeAudioPath = function () {
    const self = this;
    self.unwatchChain();
    if (!fs.existsSync(CONTRIBUTION_FILE)) return libQ.resolve();
    try { fs.unlinkSync(CONTRIBUTION_FILE); } catch (e) { return libQ.reject(e); }
    if (self.audioPath !== 'off') self.audioPath = 'off';
    return self.updateChain();
};

// The player writes its chain again as other plugins start and stop; a chain written
// without the tap while the plugin runs is noted, for the status and the journal.
Outcast.prototype.watchChain = function () {
    const self = this;
    self.unwatchChain();
    self.alsaWatcher = (curr, prev) => {
        if (!curr || !prev || curr.mtimeMs === prev.mtimeMs) return;
        let text = '';
        try { text = fs.readFileSync(ALSA_FILE, 'utf8'); } catch (e) { return; }
        if (h.chainHasTap(text)) {
            if (self.audioPath === 'lost') { self.audioPath = 'ready'; self.logger.info(LOG + 'the tap is in the audio chain again'); }
        } else if (self.audioPath === 'ready') {
            self.audioPath = 'lost';
            self.logger.warn(LOG + 'the audio chain was written without the tap');
        }
    };
    try {
        fs.watchFile(ALSA_FILE, { interval: 5000, persistent: false }, self.alsaWatcher);
    } catch (e) {
        self.logger.warn(LOG + 'chain watch: ' + msg(e));
    }
};

Outcast.prototype.unwatchChain = function () {
    if (this.alsaWatcher) {
        try { fs.unwatchFile(ALSA_FILE, this.alsaWatcher); } catch (e) { /* not watched */ }
        this.alsaWatcher = null;
    }
};

// ---------------------------------------------------------------------- core

Outcast.prototype.systemctl = function (verb) {
    const self = this;
    const defer = libQ.defer();
    execFile('/usr/bin/sudo', ['/bin/systemctl', verb, SERVICE], { timeout: 20000 }, (error, stdout, stderr) => {
        if (error) {
            self.logger.error(LOG + 'systemctl ' + verb + ': ' + String(stderr || error.message).trim());
            defer.reject(new Error(String(stderr || error.message).trim()));
        } else {
            defer.resolve();
        }
    });
    return defer.promise;
};

Outcast.prototype.startCore = function () {
    return this.systemctl('restart');
};

Outcast.prototype.stopCore = function () {
    return this.systemctl('stop');
};

Outcast.prototype.readStatus = function () {
    try { return JSON.parse(fs.readFileSync(STATUS_FILE, 'utf8')); } catch (e) { return null; }
};

// One line to the core's control socket; nothing waits on the answer.
Outcast.prototype.tellCore = function (request) {
    const c = net.connect(CONTROL_SOCKET);
    c.on('error', () => { /* the core is not up: nothing to tell */ });
    c.end(JSON.stringify(request) + '\n');
};

// The uplinks as the core reads them, from the configuration, written whole and moved
// into place; the file holds the source password, so it is the plugin's alone.
Outcast.prototype.writeUplinks = function () {
    const self = this;
    const text = JSON.stringify(h.uplinksFrom((k, d) => self.config.get(k, d)), null, 2) + '\n';
    const part = self.uplinksFile + '.part';
    fs.writeFileSync(part, text, { mode: 0o600 });
    fs.renameSync(part, self.uplinksFile);
};

// ----------------------------------------------------------------- the title

Outcast.prototype.subscribeState = function () {
    if (this.stateCallback) return;
    this.stateCallback = (state) => this.onVolumioState(state);
    this.commandRouter.addCallback('volumioPushState', this.stateCallback);
};

// The backend has no removeCallback; a left-over closure would outlive this instance.
Outcast.prototype.unsubscribeState = function () {
    const all = this.commandRouter.callbacks;
    if (this.stateCallback && all && Array.isArray(all.volumioPushState)) {
        all.volumioPushState = all.volumioPushState.filter((f) => f !== this.stateCallback);
    }
    this.stateCallback = null;
};

// Every state push while playing is handed to the core, which carries a title on only
// where it differs from the one an uplink has.
Outcast.prototype.onVolumioState = function (state) {
    if (!this.active || !state || state.status !== 'play') return;
    const title = h.screenTitle(state);
    if (!title) return;
    if (title !== this.lastLoggedTitle) {
        this.lastLoggedTitle = title;
        this.logger.info(LOG + 'title: ' + title);
    }
    this.tellCore({ op: 'title', title });
};

// ------------------------------------------------------------------ the page

function setValue(content, id, value) {
    const item = content.find((c) => c.id === id);
    if (item) item.value = value;
}

function setSelect(content, id, value) {
    const item = content.find((c) => c.id === id);
    if (!item) return;
    const option = (item.options || []).find((o) => String(o.value) === String(value));
    if (option) item.value = { value: option.value, label: option.label };
}

Outcast.prototype.getUIConfig = function () {
    const self = this;
    const defer = libQ.defer();
    const lang = self.commandRouter.sharedVars.get('language_code');
    const c = self.config;
    self.commandRouter.i18nJson(path.join(__dirname, 'i18n', 'strings_' + lang + '.json'),
        path.join(__dirname, 'i18n', 'strings_en.json'),
        path.join(__dirname, 'UIConfig.json'))
        .then((uiconf) => {
            const section = (id) => (uiconf.sections.find((s) => s.id === id) || { content: [] }).content;
            const ice = section('icecast_section');
            setValue(ice, 'ice_enabled', c.get('ice_enabled', false) === true);
            ['ice_host', 'ice_port', 'ice_mount', 'ice_user', 'ice_password', 'ice_name', 'ice_description', 'ice_genre', 'ice_url']
                .forEach((k) => setValue(ice, k, String(c.get(k, ''))));
            setSelect(ice, 'ice_codec', c.get('ice_codec', 'mp3'));
            setSelect(ice, 'ice_bitrate', c.get('ice_bitrate', '192'));
            setSelect(ice, 'ice_rate', c.get('ice_rate', '44100'));
            setValue(ice, 'ice_more', c.get('ice_more', false) === true);
            setValue(ice, 'ice_public', c.get('ice_public', false) === true);

            const lan = section('lan_section');
            setValue(lan, 'lan_enabled', c.get('lan_enabled', false) === true);
            setValue(lan, 'lan_port', String(c.get('lan_port', '8000')));
            setSelect(lan, 'lan_codec', c.get('lan_codec', 'mp3'));
            setSelect(lan, 'lan_bitrate', c.get('lan_bitrate', '192'));
            setSelect(lan, 'lan_rate', c.get('lan_rate', '44100'));
            setValue(lan, 'lan_name', String(c.get('lan_name', 'Volumio')));
            setValue(lan, 'lan_max_listeners', String(c.get('lan_max_listeners', '5')));

            const rec = section('record_section');
            setValue(rec, 'rec_enabled', c.get('rec_enabled', false) === true);
            setValue(rec, 'rec_folder', String(c.get('rec_folder', '/data/INTERNAL/outcast/recordings')));
            setSelect(rec, 'rec_codec', c.get('rec_codec', 'flac'));
            setSelect(rec, 'rec_bitrate', c.get('rec_bitrate', '192'));
            setSelect(rec, 'rec_rate', c.get('rec_rate', '0'));
            setSelect(rec, 'rec_split', c.get('rec_split', 'track'));
            setValue(rec, 'rec_title_in_name', c.get('rec_title_in_name', true) !== false);
            defer.resolve(uiconf);
        })
        .fail((e) => {
            self.logger.error(LOG + 'getUIConfig: ' + msg(e));
            defer.reject(new Error());
        });
    return defer.promise;
};

// A section saved: checked, kept, written for the core, and the core told.
Outcast.prototype.saveSection = function (data, validate, apply) {
    const self = this;
    const err = validate(data, (k, v) => self.i18n(k, v));
    if (err) {
        self.toast('error', err);
        return libQ.reject(new Error(err));
    }
    try {
        apply(data, (k, v) => self.config.set(k, v));
        self.writeUplinks();
    } catch (e) {
        self.logger.error(LOG + 'save: ' + msg(e));
        self.toast('error', 'ERR_SAVE', { error: msg(e) });
        return libQ.reject(e);
    }
    self.tellCore({ op: 'reload' });
    self.toast('success', 'SAVED');
    return libQ.resolve();
};

Outcast.prototype.updateIcecast = function (data) {
    return this.saveSection(data, h.validateIcecast, h.applyIcecast);
};

Outcast.prototype.updateLan = function (data) {
    return this.saveSection(data, h.validateLan, h.applyLan);
};

Outcast.prototype.updateRecord = function (data) {
    return this.saveSection(data, h.validateRecord, h.applyRecord);
};

Outcast.prototype.showStatus = function () {
    const self = this;
    const lines = h.summarize(self.readStatus(), self.audioPath, self.audioPathError, (key, vars) => self.i18n(key, vars));
    self.commandRouter.pushToastMessage('info', self.i18n('STATUS_TITLE'), lines.join(' '));
    return libQ.resolve();
};

// A string of the plugin's own dictionary, with its variables filled; a string that is
// not a key (a validation message already made) passes as it is.
Outcast.prototype.i18n = function (key, vars) {
    let s = this.commandRouter.getI18nString('OUTCAST.' + key);
    if (!s || s === 'OUTCAST.' + key) s = key;
    return h.fill(s, vars);
};

Outcast.prototype.toast = function (type, key, vars) {
    this.commandRouter.pushToastMessage(type, this.i18n('PLUGIN_NAME'), this.i18n(key, vars));
};

module.exports = Outcast;

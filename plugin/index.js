'use strict';
// Outcast Uplink: the plugin shell. It owns the audio path (the tap's ALSA contribution,
// proved with the player's own ALSA before it is registered and taken back when the
// chain does not take it), the core's service, the title from the player's state, and
// the settings page. Each uplink's settings and status come with its stage; the core
// (bin/outcast) carries the sound.

const libQ = require('kew');
const fs = require('fs');
const net = require('net');
const path = require('path');
const { execFile } = require('child_process');
const VConf = require('v-conf');
const { screenTitle, checkConfig, chainHasTap, summarize, fill } = require('./helpers');

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
}

// ----------------------------------------------------------------- lifecycle

Outcast.prototype.onVolumioStart = function () {
    this.configFile = this.commandRouter.pluginManager.getConfigurationFile(this.context, 'config.json');
    this.config.loadFile(this.configFile);
    return libQ.resolve();
};

Outcast.prototype.onStart = function () {
    const self = this;
    self.active = true;
    self.commandRouter.loadI18nStrings();
    self.subscribeState();
    // The core runs whatever the audio path's fate: its status says what it sees.
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
        fs.writeFileSync(CHECK_CONF, checkConfig(LIBRARY, CHECK_TAG));
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
    if (!chainHasTap(text)) {
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
        if (chainHasTap(text)) {
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

// Every state push while playing is handed to the core, which sends a title on only
// where it differs from the one an uplink carries.
Outcast.prototype.onVolumioState = function (state) {
    if (!this.active || !state || state.status !== 'play') return;
    const title = screenTitle(state);
    if (!title) return;
    if (title !== this.lastLoggedTitle) {
        this.lastLoggedTitle = title;
        this.logger.info(LOG + 'title: ' + title);
    }
    this.tellCore({ op: 'title', title });
};

// ------------------------------------------------------------------ the page

Outcast.prototype.getUIConfig = function () {
    const self = this;
    const defer = libQ.defer();
    const lang = self.commandRouter.sharedVars.get('language_code');
    self.commandRouter.i18nJson(path.join(__dirname, 'i18n', 'strings_' + lang + '.json'),
        path.join(__dirname, 'i18n', 'strings_en.json'),
        path.join(__dirname, 'UIConfig.json'))
        .then((uiconf) => defer.resolve(uiconf))
        .fail((e) => {
            self.logger.error(LOG + 'getUIConfig: ' + msg(e));
            defer.reject(new Error());
        });
    return defer.promise;
};

Outcast.prototype.showStatus = function () {
    const self = this;
    const lines = summarize(self.readStatus(), self.audioPath, self.audioPathError, (key, vars) => self.i18n(key, vars));
    self.commandRouter.pushToastMessage('info', self.i18n('STATUS_TITLE'), lines.join(' '));
    return libQ.resolve();
};

Outcast.prototype.i18n = function (key, vars) {
    let s = this.commandRouter.getI18nString('OUTCAST.' + key);
    if (!s || s === 'OUTCAST.' + key) s = key;
    return fill(s, vars);
};

Outcast.prototype.toast = function (type, key, vars) {
    this.commandRouter.pushToastMessage(type, this.i18n('PLUGIN_NAME'), this.i18n(key, vars));
};

module.exports = Outcast;

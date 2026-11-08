'use strict';

// DJ mode: the programme as blocks over the day. Each block names a source the player
// has (its own queue, a playlist, a web radio), a start time, random or in order, and
// whether it starts on the clock (hard) or at the end of the track playing (soft).
// The blocks repeat daily; the block in force is the one with the latest start at or
// before the time of day, wrapping from the day before. The scheduler decides, the
// plugin acts through the player; the presenter's hand (a queue changed by someone
// else) makes the scheduler yield until the next block.

const DAY = 86400;
const KINDS = ['queue', 'playlist', 'webradio'];

// "HH:MM" as seconds of the day, or null.
function parseStart(s) {
    const m = /^(\d{1,2}):(\d{2})$/.exec(String(s === undefined || s === null ? '' : s).trim());
    if (!m) return null;
    const h = Number(m[1]); const min = Number(m[2]);
    if (h > 23 || min > 59) return null;
    return h * 3600 + min * 60;
}

// Seconds of the day of a Date, in the player's own local time.
function secondsOfDay(date) {
    return date.getHours() * 3600 + date.getMinutes() * 60 + date.getSeconds();
}

function starts(blocks) {
    return (blocks || []).map((b) => ({ block: b, start: parseStart(b.start) })).filter((x) => x.start !== null);
}

// The block in force at a time of day: the latest start at or before it, else the
// latest of the day (it began yesterday). Null without blocks.
function blockAt(blocks, t) {
    const list = starts(blocks).sort((a, b) => a.start - b.start);
    if (!list.length) return null;
    let found = null;
    for (const x of list) if (x.start <= t) found = x.block;
    return found || list[list.length - 1].block;
}

// The next block to start after a time of day, and in how many seconds.
function nextBlock(blocks, t) {
    const list = starts(blocks).sort((a, b) => a.start - b.start);
    if (!list.length) return null;
    const later = list.find((x) => x.start > t);
    if (later) return { block: later.block, inSeconds: later.start - t };
    return { block: list[0].block, inSeconds: list[0].start + DAY - t };
}

// A queue as one string, to see it changed.
function signature(queue) {
    return (Array.isArray(queue) ? queue : []).map((i) => (i && i.uri) || '').join('\n');
}

// The programme as saved: checked; the message's key, or null.
function validate(dj) {
    if (!dj || typeof dj !== 'object') return 'ERR_DJ_SHAPE';
    const blocks = Array.isArray(dj.blocks) ? dj.blocks : [];
    const seen = new Set();
    for (const b of blocks) {
        const start = parseStart(b.start);
        if (start === null) return 'ERR_DJ_START';
        if (seen.has(start)) return 'ERR_DJ_SAME_START';
        seen.add(start);
        const name = String(b.name === undefined || b.name === null ? '' : b.name).trim();
        if (/["\n\r]/.test(name)) return 'ERR_DJ_NAME';
        const src = b.source || {};
        if (!KINDS.includes(src.kind)) return 'ERR_DJ_SOURCE';
        if (src.kind === 'playlist' && !String(src.name || '').trim()) return 'ERR_DJ_PLAYLIST';
        if (src.kind === 'webradio' && !/^https?:\/\/\S+$/.test(String(src.uri || '').trim())) return 'ERR_DJ_URI';
    }
    return null;
}

// The programme as kept: the same shape every time, ids given, blocks in order.
function tidy(dj) {
    const blocks = (Array.isArray(dj && dj.blocks) ? dj.blocks : [])
        .map((b, i) => {
            const src = b.source || {};
            return {
                id: String(b.id || ('b' + (i + 1))),
                start: (() => { const t = parseStart(b.start); return String(Math.floor(t / 3600)).padStart(2, '0') + ':' + String(Math.floor((t % 3600) / 60)).padStart(2, '0'); })(),
                name: String(b.name || '').trim() || (src.kind === 'playlist' ? String(src.name || '').trim() : src.kind === 'webradio' ? String(src.title || src.uri || '').trim() : 'The player\'s queue'),
                source: src.kind === 'playlist' ? { kind: 'playlist', name: String(src.name || '').trim() }
                    : src.kind === 'webradio' ? { kind: 'webradio', uri: String(src.uri || '').trim(), title: String(src.title || '').trim() }
                        : { kind: 'queue' },
                random: b.random === true,
                hard: b.hard === true
            };
        })
        .sort((a, b) => parseStart(a.start) - parseStart(b.start));
    return { enabled: !!(dj && dj.enabled === true), blocks };
}

// The decisions, held between ticks: what was last acted on, what is pending a track
// end, and the block yielded to the presenter.
class Scheduler {
    constructor() {
        this.acted = null;    // { id, at, signature|null }
        this.pending = null;  // { id, uri }
        this.yielded = null;  // a block id
    }

    // What to do now: { action: 'act' | 'wait' | 'none', block, reason }.
    //   now: a Date; state: the player's { status, uri }; queue: the player's queue items.
    decide(dj, now, state, queue) {
        if (!dj || !dj.enabled || !Array.isArray(dj.blocks) || !dj.blocks.length) {
            this.pending = null;
            return { action: 'none', block: null, reason: 'off' };
        }
        const block = blockAt(dj.blocks, secondsOfDay(now));
        if (!block) return { action: 'none', block: null, reason: 'off' };
        // The presenter's hand: the queue is not the one we set.
        if (this.acted && this.acted.id === block.id && this.acted.signature !== null && queue && signature(queue) !== this.acted.signature) {
            this.yielded = block.id;
        }
        if (this.yielded === block.id) return { action: 'none', block, reason: 'yielded' };
        if (this.acted && this.acted.id === block.id) return { action: 'none', block, reason: 'done' };
        if (block.source.kind === 'queue') {
            // The player's own queue: nothing to do but note the block.
            this.acted = { id: block.id, at: now.getTime(), signature: null };
            this.pending = null;
            return { action: 'none', block, reason: 'queue' };
        }
        const playing = state && state.status === 'play';
        if (block.hard || !playing) return { action: 'act', block, reason: block.hard ? 'hard' : 'idle' };
        if (!this.pending || this.pending.id !== block.id) {
            this.pending = { id: block.id, uri: state.uri || '' };
            return { action: 'wait', block, reason: 'track' };
        }
        if ((state.uri || '') !== this.pending.uri) return { action: 'act', block, reason: 'boundary' };
        return { action: 'wait', block, reason: 'track' };
    }

    // The block acted on; the queue's signature comes with the first settle after.
    didAct(block, now) {
        this.acted = { id: block.id, at: now.getTime(), signature: null };
        this.pending = null;
        if (this.yielded === block.id) this.yielded = null;
    }

    // The queue as it stands after our action, remembered once.
    settle(queue) {
        if (this.acted && this.acted.signature === null && Array.isArray(queue) && queue.length) {
            this.acted.signature = signature(queue);
        }
    }

    // The presenter hands back: the block in force is acted on again.
    takeOver() {
        this.acted = null;
        this.pending = null;
        this.yielded = null;
    }

    // For the status: what the scheduler is doing.
    report(dj, now) {
        const t = secondsOfDay(now);
        const block = dj && dj.enabled ? blockAt(dj.blocks, t) : null;
        const next = dj && dj.enabled ? nextBlock(dj.blocks, t) : null;
        return {
            enabled: !!(dj && dj.enabled),
            block: block ? block.name : '',
            block_start: block ? block.start : '',
            next: next ? next.block.name : '',
            next_in: next ? next.inSeconds : 0,
            yielded: !!(block && this.yielded === block.id),
            acted_at: this.acted ? Math.floor(this.acted.at / 1000) : 0
        };
    }
}

module.exports = { DAY, KINDS, parseStart, secondsOfDay, blockAt, nextBlock, signature, validate, tidy, Scheduler };

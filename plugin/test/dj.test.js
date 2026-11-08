'use strict';
const test = require('node:test');
const assert = require('node:assert');
const dj = require('../dj.js');

const at = (h, m, s) => { const d = new Date(2026, 10, 8, h, m, s || 0); return d; };
const programme = {
    enabled: true,
    blocks: [
        { id: 'morning', start: '06:00', name: 'Morning', source: { kind: 'playlist', name: 'Morning' }, random: true, hard: false },
        { id: 'midday', start: '10:00', name: 'Midday', source: { kind: 'playlist', name: 'Midday' }, random: false, hard: true },
        { id: 'night', start: '22:00', name: 'Night', source: { kind: 'webradio', uri: 'http://radio.example.org/night', title: 'Night radio' }, random: false, hard: false },
        { id: 'free', start: '00:30', name: 'Free', source: { kind: 'queue' }, random: false, hard: false }
    ]
};

test('the block in force is the latest start at or before now, wrapping from yesterday', () => {
    assert.equal(dj.blockAt(programme.blocks, dj.secondsOfDay(at(7, 0))).id, 'morning');
    assert.equal(dj.blockAt(programme.blocks, dj.secondsOfDay(at(10, 0))).id, 'midday');
    assert.equal(dj.blockAt(programme.blocks, dj.secondsOfDay(at(23, 59))).id, 'night');
    assert.equal(dj.blockAt(programme.blocks, dj.secondsOfDay(at(0, 10))).id, 'night', 'before the first start the last block of the day is still on');
    assert.equal(dj.blockAt(programme.blocks, dj.secondsOfDay(at(0, 30))).id, 'free');
    assert.equal(dj.blockAt([], 0), null);
    const next = dj.nextBlock(programme.blocks, dj.secondsOfDay(at(23, 0)));
    assert.equal(next.block.id, 'free');
    assert.equal(next.inSeconds, 90 * 60);
    assert.equal(dj.nextBlock(programme.blocks, dj.secondsOfDay(at(7, 0))).block.id, 'midday');
});

test('the programme is checked and tidied', () => {
    assert.equal(dj.validate(programme), null);
    assert.equal(dj.validate({ blocks: [{ start: '25:00', source: { kind: 'queue' } }] }), 'ERR_DJ_START');
    assert.equal(dj.validate({ blocks: [{ start: '06:00', source: { kind: 'queue' } }, { start: '6:00', source: { kind: 'queue' } }] }), 'ERR_DJ_SAME_START');
    assert.equal(dj.validate({ blocks: [{ start: '06:00', source: { kind: 'tape' } }] }), 'ERR_DJ_SOURCE');
    assert.equal(dj.validate({ blocks: [{ start: '06:00', source: { kind: 'playlist', name: ' ' } }] }), 'ERR_DJ_PLAYLIST');
    assert.equal(dj.validate({ blocks: [{ start: '06:00', source: { kind: 'webradio', uri: 'radio.example.org' } }] }), 'ERR_DJ_URI');
    assert.equal(dj.validate({ blocks: [{ start: '06:00', name: 'a "quoted" name', source: { kind: 'queue' } }] }), 'ERR_DJ_NAME');
    const tidy = dj.tidy({ enabled: 'yes', blocks: [{ start: '9:05', source: { kind: 'playlist', name: ' Late ' } }, { start: '6:00', source: { kind: 'queue' }, random: true }] });
    assert.deepEqual(tidy, {
        enabled: false,
        blocks: [
            { id: 'b2', start: '06:00', name: "The player's queue", source: { kind: 'queue' }, random: true, hard: false },
            { id: 'b1', start: '09:05', name: 'Late', source: { kind: 'playlist', name: 'Late' }, random: false, hard: false }
        ]
    });
});

test('a soft block waits for the track to end, a hard one acts on the clock, an idle player acts at once', () => {
    const s = new dj.Scheduler();
    const playing = { status: 'play', uri: 'music-library/a.flac' };
    // 07:00, the morning block, soft, a track playing: wait for its end.
    assert.equal(s.decide(programme, at(7, 0), playing, []).action, 'wait');
    assert.equal(s.decide(programme, at(7, 0, 20), playing, []).action, 'wait');
    const boundary = s.decide(programme, at(7, 3), { status: 'play', uri: 'music-library/b.flac' }, []);
    assert.deepEqual([boundary.action, boundary.block.id, boundary.reason], ['act', 'morning', 'boundary']);
    s.didAct(boundary.block, at(7, 3));
    assert.equal(s.decide(programme, at(7, 4), playing, []).reason, 'done');
    // 10:00, the midday block is hard: act while the track plays.
    const hard = s.decide(programme, at(10, 0), playing, []);
    assert.deepEqual([hard.action, hard.reason], ['act', 'hard']);
    s.didAct(hard.block, at(10, 0));
    // 22:00, soft, but the player is stopped: act at once.
    const idle = s.decide(programme, at(22, 0), { status: 'stop', uri: '' }, []);
    assert.deepEqual([idle.action, idle.reason], ['act', 'idle']);
    // DJ mode off: nothing.
    assert.equal(new dj.Scheduler().decide(Object.assign({}, programme, { enabled: false }), at(7, 0), playing, []).reason, 'off');
});

test('the presenter changing the queue makes the scheduler yield until the next block, and a take-over hands it back', () => {
    const s = new dj.Scheduler();
    const ours = [{ uri: 'a' }, { uri: 'b' }];
    const d = s.decide(programme, at(10, 1), { status: 'stop' }, []);
    assert.equal(d.action, 'act');
    s.didAct(d.block, at(10, 1));
    s.settle([]);            // nothing yet: the queue fills after our action
    s.settle(ours);          // the queue as we set it
    assert.equal(s.decide(programme, at(10, 2), { status: 'play', uri: 'a' }, ours).reason, 'done');
    const theirs = [{ uri: 'a' }, { uri: 'b' }, { uri: 'the presenter\'s pick' }];
    assert.equal(s.decide(programme, at(10, 3), { status: 'play', uri: 'a' }, theirs).reason, 'yielded');
    assert.equal(s.report(programme, at(10, 3)).yielded, true);
    // the next block starts regardless of the yield
    assert.equal(s.decide(programme, at(22, 0), { status: 'stop' }, theirs).action, 'act');
    // and a take-over acts again within a block
    const s2 = new dj.Scheduler();
    const d2 = s2.decide(programme, at(10, 1), { status: 'stop' }, []); s2.didAct(d2.block, at(10, 1)); s2.settle(ours);
    s2.decide(programme, at(10, 3), { status: 'play', uri: 'a' }, theirs);
    s2.takeOver();
    assert.equal(s2.decide(programme, at(10, 4), { status: 'play', uri: 'a' }, theirs).action, 'act');
});

test('a block of the player\'s own queue leaves the player alone and never yields', () => {
    const s = new dj.Scheduler();
    const d = s.decide(programme, at(0, 45), { status: 'play', uri: 'x' }, [{ uri: 'x' }]);
    assert.deepEqual([d.action, d.reason, d.block.id], ['none', 'queue', 'free']);
    assert.equal(s.decide(programme, at(0, 50), { status: 'play', uri: 'y' }, [{ uri: 'y' }, { uri: 'z' }]).reason, 'done');
    const r = s.report(programme, at(0, 50));
    assert.deepEqual([r.enabled, r.block, r.next, r.next_in, r.yielded], [true, 'Free', 'Morning', 5 * 3600 + 10 * 60, false]);
});

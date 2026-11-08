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
        ],
        inserts: []
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

test('inserts fire on the hour, every so many tracks, or at a block start; soft ones wait for a track end or five minutes', () => {
    const inserts = dj.tidyInserts([
        { id: 'top', name: 'Top of the hour', file: 'top.wav', when: 'hour', minute: 0, duck_db: -60, hard: true },
        { id: 'id', file: 'id.wav', when: 'tracks', every: 2 },
        { id: 'sweep', file: 'sweep.wav', when: 'block', duck_db: -6 },
        { id: 'soft', file: 'soft.wav', when: 'hour', minute: 30 }
    ]);
    assert.equal(dj.validateInserts(inserts), null);
    assert.equal(inserts[1].name, 'id');
    assert.equal(dj.validateInserts([{ file: '../etc/passwd.wav', when: 'hour' }]), 'ERR_DJ_INSERT_FILE');
    assert.equal(dj.validateInserts([{ file: 'a.mp3', when: 'hour' }]), 'ERR_DJ_INSERT_FILE');
    assert.equal(dj.validateInserts([{ file: 'a.wav', when: 'sometimes' }]), 'ERR_DJ_INSERT_WHEN');
    assert.equal(dj.validateInserts([{ file: 'a.wav', when: 'hour', minute: 60 }]), 'ERR_DJ_INSERT_NUMBER');
    assert.equal(dj.validateInserts([{ file: 'a.wav', when: 'tracks', every: 0 }]), 'ERR_DJ_INSERT_NUMBER');
    const ins = new dj.Inserts();
    // 09:00:05: the hard top-of-hour fires once in the hour; not again at 09:00:20.
    assert.deepEqual(ins.tick(inserts, at(9, 0, 5), false, false).map((i) => i.id), ['top']);
    assert.deepEqual(ins.tick(inserts, at(9, 0, 20), false, false), []);
    // Two track ends: the every-two insert on the second.
    assert.deepEqual(ins.tick(inserts, at(9, 3), true, false).map((i) => i.id), []);
    assert.deepEqual(ins.tick(inserts, at(9, 7), true, false).map((i) => i.id), ['id']);
    // A block start: the sweeper.
    assert.deepEqual(ins.tick(inserts, at(10, 5), false, true).map((i) => i.id), ['sweep']);
    // 09:30 soft: waits for a track end, and takes it.
    assert.deepEqual(ins.tick(inserts, at(9, 30, 10), false, false), []);
    assert.deepEqual(ins.tick(inserts, at(9, 31), true, false).map((i) => i.id), ['soft']);
    // A soft one with no track end in five minutes fires anyway (a web radio).
    const ins2 = new dj.Inserts();
    assert.deepEqual(ins2.tick(inserts, at(11, 30, 0), false, false), []);
    assert.deepEqual(ins2.tick(inserts, at(11, 34, 50), false, false), []);
    assert.deepEqual(ins2.tick(inserts, at(11, 35, 1), false, false).map((i) => i.id), ['soft']);
});

test('a block of the player\'s own queue leaves the player alone and never yields', () => {
    const s = new dj.Scheduler();
    const d = s.decide(programme, at(0, 45), { status: 'play', uri: 'x' }, [{ uri: 'x' }]);
    assert.deepEqual([d.action, d.reason, d.block.id], ['none', 'queue', 'free']);
    assert.equal(s.decide(programme, at(0, 50), { status: 'play', uri: 'y' }, [{ uri: 'y' }, { uri: 'z' }]).reason, 'done');
    const r = s.report(programme, at(0, 50));
    assert.deepEqual([r.enabled, r.block, r.next, r.next_in, r.yielded], [true, 'Free', 'Morning', 5 * 3600 + 10 * 60, false]);
});

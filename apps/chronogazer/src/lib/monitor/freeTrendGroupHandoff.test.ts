import { describe, expect, it } from 'vitest';
import {
	groupHandoffDecision,
	offerGroupHandoff,
	takeGroupHandoff,
	type GroupHandoffNow
} from './freeTrendGroupHandoff';

const handoff = { owner: 'account:a', generation: 3, skipped: 2 };
const base: GroupHandoffNow = {
	owner: 'account:a',
	generation: 3,
	hasDraft: true,
	canCreate: true,
	createBlocked: false
};

describe('groupHandoffDecision', () => {
	const cases: [string, typeof handoff | null, Partial<GroupHandoffNow>, string][] = [
		['open', handoff, {}, 'open'],
		['owner 違い', handoff, { owner: 'account:b' }, 'stale'],
		['owner 無し', handoff, { owner: null }, 'stale'],
		['世代違い', handoff, { generation: 4 }, 'stale'],
		['下書き無し', handoff, { hasDraft: false }, 'stale'],
		['作れない', handoff, { canCreate: false }, 'blocked'],
		['上限', handoff, { createBlocked: true }, 'blocked'],
		['世代違い・上限は stale が先', handoff, { generation: 4, createBlocked: true }, 'stale'],
		[
			'owner 違い・作れないは stale が先',
			handoff,
			{ owner: 'account:b', canCreate: false },
			'stale'
		],
		['handoff 無し', null, {}, 'none']
	];
	for (const [name, h, now, want] of cases) {
		it(name, () => {
			expect(groupHandoffDecision(h, { ...base, ...now })).toBe(want);
		});
	}
});

describe('offer / take', () => {
	it('取り出すと空になる', () => {
		offerGroupHandoff(handoff);
		expect(takeGroupHandoff()).toEqual(handoff);
		expect(takeGroupHandoff()).toBeNull();
	});
});

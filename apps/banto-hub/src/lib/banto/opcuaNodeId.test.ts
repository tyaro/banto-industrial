import { describe, expect, it } from 'vitest';
import { opcUaNodeId, opcUaNodeIdLines } from './opcuaNodeId';

const ns = 'urn:banto:opcua:tags';

describe('OPC UA NodeId', () => {
	it('nsu と s で組む', () => {
		expect(opcUaNodeId(ns, 'plc1.g1.temp')).toBe('nsu=urn:banto:opcua:tags;s=plc1.g1.temp');
	});

	it('複数行は 1 行 1 件で、順序を保つ', () => {
		const lines = opcUaNodeIdLines(ns, [
			{ dataType: 'f32', externalName: 'a.b.c' },
			{ dataType: 'u16', externalName: 'a.b.d' }
		]);
		expect(lines).toEqual([`nsu=${ns};s=a.b.c`, `nsu=${ns};s=a.b.d`]);
	});

	it('string 型は除く', () => {
		const lines = opcUaNodeIdLines(ns, [
			{ dataType: 'string', externalName: 'a.b.s' },
			{ dataType: 'bool', externalName: 'a.b.f' }
		]);
		expect(lines).toEqual([`nsu=${ns};s=a.b.f`]);
		expect(opcUaNodeIdLines(ns, [{ dataType: 'string', externalName: 'x.y.z' }])).toEqual([]);
	});
});

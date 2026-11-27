import { describe, expect, it } from 'vitest';
import { opcUaNodeId, opcUaNodeIdLines } from './opcuaNodeId';

const ns = 'urn:banto:opcua:tags';

describe('OPC UA NodeId', () => {
	it('nsu と s で組む', () => {
		expect(opcUaNodeId(ns, 'plc1.g1.temp')).toBe('nsu=urn:banto:opcua:tags;s=plc1.g1.temp');
	});

	it('Namespace URI の ; は %3B に符号化する', () => {
		expect(opcUaNodeId('urn:vendor:a;b', 'plc1.g1.temp')).toBe(
			'nsu=urn:vendor:a%3Bb;s=plc1.g1.temp'
		);
	});

	it('Namespace URI の % は %25 に符号化する（%3B の文字列を取り違えない）', () => {
		expect(opcUaNodeId('urn:vendor:a%3Bb', 'plc1.g1.temp')).toBe(
			'nsu=urn:vendor:a%253Bb;s=plc1.g1.temp'
		);
	});

	it('往復: 読み手（nsu= から最初の ; まで、%3B と %25 を一度で戻す）で元の URI に戻る', () => {
		// banto-opcua の NodeAddress と同じ読み方のテスト用の写し。
		const decodeNsu = (nodeId: string): { uri: string; name: string } => {
			const rest = nodeId.slice('nsu='.length);
			const cut = rest.indexOf(';');
			const uri = rest.slice(0, cut).replace(/%3B|%25/g, (m) => (m === '%3B' ? ';' : '%'));
			const name = rest.slice(cut + ';s='.length);
			return { uri, name };
		};
		const uris = ['urn:banto:opcua:tags', 'urn:vendor:a;b', 'urn:vendor:a%3Bb', 'urn:x:100%'];
		for (const uri of uris) {
			const decoded = decodeNsu(opcUaNodeId(uri, 'plc1.g1.temp'));
			expect(decoded.uri).toBe(uri);
			expect(decoded.name).toBe('plc1.g1.temp');
		}
		expect(opcUaNodeId('urn:vendor:a;b', 'x.y.z')).not.toBe(
			opcUaNodeId('urn:vendor:a%3Bb', 'x.y.z')
		);
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

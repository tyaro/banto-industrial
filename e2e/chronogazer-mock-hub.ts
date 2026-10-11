/**
 * ChronoGazer の E2E 用の**モック Hub**（#383 段階3 P5）。
 *
 * `banto-hub` を実際に立てる代わりに、ChronoGazer の購読クライアント
 * （`banto-tagclient`）が叩く口だけを Node で答える最小の実装:
 *
 * - REST: `GET .../values`（現在値の一覧）と、それ以外のパス（タグ一覧 = catalog、
 *   到達確認）はタグ一覧を返す。応答の形は `crates/banto-tagclient` の
 *   `CatalogSnapshot` / `ValuesSnapshot`（Rust 側の統合テスト
 *   `apps/chronogazer/core/tests/hub_collect_roundtrip.rs` のモック Hub と同じ形）。
 * - WebSocket: 手書きの最小実装（`ws` パッケージを足すと `pnpm-lock.yaml` が
 *   動くので使わない）。購読要求（クライアントの最初のテキストフレーム）に
 *   `data` フレームを 1 つ返し、閉じられるまで開けておく。
 *
 * `stop()` は開いている接続をすべて切ってから待受を閉じる（「Hub が落ちた」を
 * 作る）。同じポートで `MockHub.start(tags, { port })` し直せば「Hub が戻った」を
 * 作れる。
 *
 * 固定ポートは使わない（既定は空きポートを OS に選ばせる）。
 */
import crypto from 'node:crypto';
import http from 'node:http';
import type { Duplex } from 'node:stream';

export interface MockHubTag {
	/** Hub の外部名（`接続名.グループ名.タグ名`）。 */
	externalName: string;
	dataType: string;
	value: number;
	/** Hub の `value_source`（`real` / `simulation` / `derived_simulation` / 未知の文字列）。 */
	valueSource: string;
}

const WS_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';

function catalogJson(tags: MockHubTag[]): string {
	return JSON.stringify({
		revision: 1,
		run_id: 1,
		collection_mode: 'configured',
		tags: tags.map((tag, index) => {
			const parts = tag.externalName.split('.');
			return {
				external_name: tag.externalName,
				tag_key: `tag:${index + 1}`,
				ids: [1, 1, index + 1],
				connection: parts[0] ?? 'HUB',
				group: parts[1] ?? 'G',
				name: parts[parts.length - 1],
				address: 'D100',
				data_type: tag.dataType,
				unit: null,
				decimals: 0,
				period_ms: 500,
				enabled: true,
				writable: false,
				tag_kind: 'plc',
				expression: null,
				retain: false,
				simulation: false,
				configured_simulation: false,
				effective_simulation: false,
				value_source: tag.valueSource
			};
		})
	});
}

function valuesJson(tags: MockHubTag[]): string {
	const t = Date.now();
	return JSON.stringify({
		revision: 1,
		t,
		run_id: 1,
		collection_mode: 'configured',
		values: tags.map((tag) => ({
			tag: tag.externalName,
			v: tag.value,
			q: 'good',
			t,
			value_source: tag.valueSource
		}))
	});
}

/** サーバー → クライアントのテキストフレーム（マスク無し）。 */
function textFrame(text: string): Buffer {
	const payload = Buffer.from(text, 'utf8');
	let header: Buffer;
	if (payload.length < 126) {
		header = Buffer.from([0x81, payload.length]);
	} else if (payload.length < 65536) {
		header = Buffer.from([0x81, 126, payload.length >> 8, payload.length & 0xff]);
	} else {
		throw new Error('モック Hub: 65535 バイトを超えるフレームは作らない');
	}
	return Buffer.concat([header, payload]);
}

interface ClientFrame {
	opcode: number;
	payload: Buffer;
}

/** `buffer` の先頭から（マスク付きの）クライアントのフレームを 1 つ取り出す。足りなければ `null`。 */
function takeClientFrame(buffer: Buffer): { frame: ClientFrame; rest: Buffer } | null {
	if (buffer.length < 2) return null;
	const opcode = buffer[0] & 0x0f;
	let length = buffer[1] & 0x7f;
	let offset = 2;
	if (length === 126) {
		if (buffer.length < 4) return null;
		length = buffer.readUInt16BE(2);
		offset = 4;
	} else if (length === 127) {
		if (buffer.length < 10) return null;
		length = Number(buffer.readBigUInt64BE(2));
		offset = 10;
	}
	const masked = (buffer[1] & 0x80) !== 0;
	const maskLength = masked ? 4 : 0;
	if (buffer.length < offset + maskLength + length) return null;
	const mask = masked ? buffer.subarray(offset, offset + 4) : null;
	const payload = Buffer.from(buffer.subarray(offset + maskLength, offset + maskLength + length));
	if (mask) {
		for (let i = 0; i < payload.length; i++) payload[i] ^= mask[i % 4];
	}
	return { frame: { opcode, payload }, rest: buffer.subarray(offset + maskLength + length) };
}

export class MockHub {
	private readonly sockets = new Set<Duplex>();

	private constructor(
		private readonly server: http.Server,
		readonly port: number
	) {}

	/** 待受を始める。`port` を省略すると空きポートを OS に選ばせる。 */
	static async start(tags: MockHubTag[], options: { port?: number } = {}): Promise<MockHub> {
		const server = http.createServer((req, res) => {
			const body = (req.url ?? '').includes('/api/v1/values')
				? valuesJson(tags)
				: catalogJson(tags);
			res.writeHead(200, {
				'Content-Type': 'application/json',
				'Content-Length': Buffer.byteLength(body),
				Connection: 'close'
			});
			res.end(body);
		});
		const hubRef: { current: MockHub | null } = { current: null };
		server.on('connection', (socket) => {
			hubRef.current?.sockets.add(socket);
			socket.on('close', () => hubRef.current?.sockets.delete(socket));
		});
		server.on('upgrade', (req, socket) => {
			const key = req.headers['sec-websocket-key'];
			if (typeof key !== 'string') {
				socket.destroy();
				return;
			}
			const accept = crypto
				.createHash('sha1')
				.update(key + WS_GUID)
				.digest('base64');
			socket.write(
				[
					'HTTP/1.1 101 Switching Protocols',
					'Upgrade: websocket',
					'Connection: Upgrade',
					`Sec-WebSocket-Accept: ${accept}`,
					'',
					''
				].join('\r\n')
			);
			let pending: Buffer = Buffer.alloc(0);
			let answered = false;
			socket.on('data', (chunk: Buffer) => {
				pending = Buffer.concat([pending, chunk]);
				for (;;) {
					const taken = takeClientFrame(pending);
					if (!taken) return;
					pending = taken.rest;
					const { opcode, payload } = taken.frame;
					if (opcode === 0x8) {
						// close: 同じ内容で返して閉じる。
						socket.end(Buffer.concat([Buffer.from([0x88, payload.length]), payload]));
						return;
					}
					if (opcode === 0x9) {
						socket.write(Buffer.concat([Buffer.from([0x8a, payload.length]), payload]));
						continue;
					}
					if (opcode === 0x1 && !answered) {
						answered = true;
						let id = 1;
						try {
							const parsed = JSON.parse(payload.toString('utf8')) as { id?: number };
							if (typeof parsed.id === 'number') id = parsed.id;
						} catch {
							// 購読要求として読めなくても、既定の id で返す。
						}
						const t = Date.now();
						socket.write(
							textFrame(
								JSON.stringify({
									op: 'data',
									id,
									t,
									values: tags.map((tag) => ({ tag: tag.externalName, v: tag.value, q: 'good', t }))
								})
							)
						);
					}
				}
			});
			socket.on('error', () => socket.destroy());
		});
		await new Promise<void>((resolve, reject) => {
			server.once('error', reject);
			server.listen(options.port ?? 0, '127.0.0.1', () => resolve());
		});
		const address = server.address();
		if (address === null || typeof address === 'string') {
			throw new Error('モック Hub のポートを取得できませんでした');
		}
		const hub = new MockHub(server, address.port);
		hubRef.current = hub;
		return hub;
	}

	get endpoint(): string {
		return `http://127.0.0.1:${this.port}`;
	}

	/** 開いている接続をすべて切り、待受を閉じる（閉じるまで待つ）。 */
	async stop(): Promise<void> {
		for (const socket of this.sockets) socket.destroy();
		this.sockets.clear();
		await new Promise<void>((resolve) => this.server.close(() => resolve()));
	}
}

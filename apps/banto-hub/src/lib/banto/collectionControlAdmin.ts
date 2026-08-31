/**
 * `admin` 限定の収集ライフサイクル制御クライアント（2026-08-31 オーナー指摘、
 * T14-4）。`apps/banto-hub/core/src/rest.rs` の
 * `POST /api/collection/start|start-all-simulation|stop` に対応する -
 * `writeControlAdmin.ts` と同じ httpPost 雛形（レスポンス型が違うだけ）。
 *
 * **なぜこのファイルが要る（実装指示の背景）**: `rest.rs` の
 * `commit_catalog_and_notify` の doc comment のとおり、本番経路では
 * `legacy_live_reconfigure` は無効で「registry writes advance the
 * configured revision only」 - PLC接続・収集グループ・タグを作成/変更しても
 * configured revision が上がるだけで、動いている収集機（あるいはまだ一度も
 * 開始していない収集機）には反映されない。反映させるには収集を
 * `RunMode::Configured`（実機）で開始/再開始する必要があるが、
 * `POST /api/collection/start` 自体は元々 API にしか無く、UI から叩く導線が
 * 1つも無かった - 実機での試運転の最後の一歩「PLC に接続開始し、タグに
 * アクセスできているか確認する」がまさにこの未実装導線を必要としていた。
 * このファイルはその導線用クライアント（`(app)/status/+page.svelte` から
 * 使う）。
 *
 * 現在の収集状態自体（`collectionState`/`collectionMode`）は
 * `GET /api/status`（`hubStatus.ts`）を見る - このファイルは
 * start/start-all-simulation/stop の**操作**のみを提供する
 * （`writeControlAdmin.ts` と同じ「状態は読む専用 API、操作は専用 API」の
 * 分離方針）。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

/**
 * `POST /api/collection/start|start-all-simulation|stop` の応答。
 * Mirrors `banto_hub_core::rest::CollectionStatusResponse`
 * (`#[serde(rename_all = "camelCase")]`)。
 */
export interface CollectionStatusResponse {
	state: string;
	mode: string;
	runId: number | null;
	configuredRevision: number;
	runningRevision: number;
	lastError: string | null;
}

const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

const ERROR_KINDS = new Set([
	'not_found',
	'validation',
	'unauthorized',
	'forbidden',
	'storage',
	'other'
]);

function isErrorBody(value: unknown): value is ErrorBody {
	if (typeof value !== 'object' || value === null) return false;
	const kind = (value as { kind?: unknown }).kind;
	return typeof kind === 'string' && ERROR_KINDS.has(kind);
}

function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

async function httpPost(path: string): Promise<CollectionStatusResponse> {
	const headers: Record<string, string> = { ...CSRF_HEADER };
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch(path, { method: 'POST', headers });
	} catch {
		throw new ProviderError({ kind: 'other', message: NETWORK_ERROR_MESSAGE });
	}

	if (!response.ok) {
		let body: unknown;
		try {
			body = await response.json();
		} catch {
			throw new ProviderError({
				kind: 'other',
				message: `${response.status} ${response.statusText}`
			});
		}
		if (isErrorBody(body)) throw new ProviderError(body);
		throw new ProviderError({
			kind: 'other',
			message: `${response.status} ${response.statusText}`
		});
	}

	return (await response.json()) as CollectionStatusResponse;
}

/** 設定どおり収集を開始する（`RunMode::Configured` = 実機）。 */
export async function startCollection(): Promise<CollectionStatusResponse> {
	return httpPost('/api/collection/start');
}

/**
 * 全 PLC シミュレーションを開始する（`RunMode::AllSimulation`）。
 *
 * **主動線ではなく副次的な選択肢**（2026-08-31 オーナー指摘）:
 * 接続単位のシミュレーション（`PlcConnection.simulation`、T9-2、接続の
 * Drawer にあるチェックボックス）とは別物 - こちらは「実 PLC へ一切接続せず
 * 全タグをシミュレータ値にする」運転モード全体の切替。実機が手元に無い
 * 環境での確認には引き続き価値があるためボタン自体は残すが、初回
 * チェックリストの必須ステップからは外した（実機がある試運転では SIM を
 * 挟む必然性が無いため - `tagOnboarding.ts` 冒頭のdoc comment参照）。
 */
export async function startAllSimulationCollection(): Promise<CollectionStatusResponse> {
	return httpPost('/api/collection/start-all-simulation');
}

/** 収集を停止する（履歴 flush、PLC 接続と通常の外部出力を停止）。 */
export async function stopCollection(): Promise<CollectionStatusResponse> {
	return httpPost('/api/collection/stop');
}

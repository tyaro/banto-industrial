/**
 * ChronoGazer の口の模擬（設計 docs/demo-pages-design.md §3.3）。今は `/tags`・`/monitor` を
 * 開いたときの起動で呼ばれる一覧を空で返すだけ（登録・値・履歴は後の PR）。
 *
 * どの口が要るかは、デモのビルドで両方の画面を開き、模擬の「口がありません」の警告から
 * 拾った（2026-11-22）。応答の型は各 `*Admin.ts` から取る（模擬の側で書き直さない）。
 */
import { json, type DemoHttpRoute } from '@banto-industrial/demo-backend';
import type { DisplayGroup } from '#lib/banto/displayGroupsAdmin.js';
import type { TagThresholds } from '#lib/banto/tagThresholdsAdmin.js';
import type {
	CollectionGroup,
	DriverInfo,
	ExclusionView,
	PlcConnection,
	Tag
} from '#lib/banto/tagRegistryAdmin.js';

export function createChronoGazerRoutes(): DemoHttpRoute[] {
	const drivers: DriverInfo[] = [];
	const plcConnections: PlcConnection[] = [];
	const collectionGroups: CollectionGroup[] = [];
	const tags: Tag[] = [];
	const displayGroups: DisplayGroup[] = [];
	const tagThresholds: TagThresholds[] = [];
	const configExclusions: ExclusionView[] = [];
	return [
		{ method: 'GET', path: '/api/drivers', handle: () => json(drivers) },
		{ method: 'GET', path: '/api/plc-connections', handle: () => json(plcConnections) },
		{ method: 'GET', path: '/api/collection-groups', handle: () => json(collectionGroups) },
		{ method: 'GET', path: '/api/tags', handle: () => json(tags) },
		{ method: 'GET', path: '/api/display-groups', handle: () => json(displayGroups) },
		{ method: 'GET', path: '/api/tag-thresholds', handle: () => json(tagThresholds) },
		{ method: 'GET', path: '/api/config-exclusions', handle: () => json(configExclusions) }
	];
}

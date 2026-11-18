<script lang="ts">
	/**
	 * ページの `beforeNavigate` を、そのページの中のどの離脱の確認
	 * （`guardUnsavedChanges`）よりも先に走らせるための、描くものの無い部品
	 * （#1179 PR4b）。ページのテンプレートの一番最初に置く。
	 *
	 * なぜ要るか:
	 * - SvelteKit の `beforeNavigate` は、呼んだ部品の `onMount` で登録され、
	 *   登録の順に呼ばれる。子は親より先に mount するので、ページ自身の
	 *   `beforeNavigate` は、子（例: 「記録」ページの `HubImportDialog`）が中で
	 *   呼ぶ `guardUnsavedChanges` の登録より後になる。
	 * - `@banto/forms` の離脱の確認は、最初に走ったコールバックが登録簿全体を見て
	 *   答える。そのため、ページが「この移動はページ内の切り替えなので確かめ済み」
	 *   と印を立てる前に、汎用の確認（「この画面から移動しますか」）が出てしまう。
	 * - 兄弟の `onMount` は描いた順に走るので、この部品を最初に描けば、ここで
	 *   登録した `beforeNavigate` がページの中の誰よりも先に走る。
	 *
	 * tyaro/banto#675（最初のコールバックが全体に答える問題）が直れば不要になる。
	 */
	import { beforeNavigate, type BeforeNavigate } from '$app/navigation';

	interface Props {
		onbeforenavigate: (navigation: BeforeNavigate) => void;
	}

	let { onbeforenavigate }: Props = $props();

	beforeNavigate((navigation) => onbeforenavigate(navigation));
</script>

<script lang="ts">
	/**
	 * CSV 出力・日報に出すペンの選択（trend-design.md §6）。判断は `penSelectionLogic.ts`。
	 * 上限（8 本）を超えるペンを持つグループのときだけ、呼び出し側が出す。
	 */
	import {
		PEN_SELECTION_NOTICE,
		isPenCheckDisabled,
		penSelectionCountLabel,
		uniqueTagIds
	} from '#lib/penSelection/penSelectionLogic.js';

	interface Props {
		/** グループのペンのタグ ID（グループの順）。 */
		tagIds: readonly number[];
		/** いま選んでいるタグ ID。 */
		selected: readonly number[];
		limit: number;
		label: (tagId: number) => string;
		disabled?: boolean;
		ontoggle: (tagId: number) => void;
	}

	let { tagIds, selected, limit, label, disabled = false, ontoggle }: Props = $props();
</script>

<fieldset class="pen-select" {disabled}>
	<legend>出すペン</legend>
	<p class="notice" role="status">{PEN_SELECTION_NOTICE}</p>
	<ul>
		{#each uniqueTagIds(tagIds) as tagId (tagId)}
			<li>
				<label>
					<input
						type="checkbox"
						checked={selected.includes(tagId)}
						disabled={isPenCheckDisabled(selected, tagId, limit)}
						onchange={() => ontoggle(tagId)}
					/>
					{label(tagId)}
				</label>
			</li>
		{/each}
	</ul>
	<p class="count">{penSelectionCountLabel(selected.length, limit)}</p>
</fieldset>

<style>
	.pen-select {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		padding: 0.5rem 0.75rem;
		min-width: 0;
	}

	.notice {
		margin: 0;
		font-size: 0.85rem;
	}

	ul {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(12rem, 1fr));
		gap: 0.2rem 0.75rem;
		max-height: 12rem;
		overflow-y: auto;
	}

	label {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		font-size: 0.85rem;
	}

	.count {
		margin: 0;
		font-size: 0.8rem;
		color: var(--banto-text-muted);
	}
</style>

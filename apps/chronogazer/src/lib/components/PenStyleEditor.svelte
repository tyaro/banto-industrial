<script lang="ts">
	/**
	 * ペン 1 本の属性の編集欄「トレンドの表示」（#1207・#1231 PR 8b2b、
	 * docs/chronogazer/trend-design.md §5・§10 の 6b・7b・7c・8b2b）。表示グループのページと
	 * 自由トレンドの画面が同じ部品を使う。
	 *
	 * - 表示グループの型（`hubTag`・登録先）には結びつけない。受け取るのはペンの属性（`PenStyle`）、
	 *   数の欄の入力の文字（`texts`）、タグの情報（名前・単位・bit か）、しきい値（管理線の
	 *   placeholder）、その行の誤り、無効化。
	 * - 変更は `onchange(patch)`（属性の差分。空の選択は `null`）と、数の欄の入力の文字の
	 *   `ontext(field, text)` で知らせる。下書きへの当て方（数として読めた値だけを入れる、など）は
	 *   画面が決める。検証は `penStyleEditor.ts` の `penStyleProblems`。
	 * - その行の誤り（`errors`）は欄の後ろに出す（表示グループのページの並びのまま）。
	 */
	import {
		DEFAULT_BIT_BAND_HEIGHT,
		DEFAULT_LINE_WIDTH,
		DEFAULT_MARKER_SIZE,
		type PenStyle
	} from '#lib/banto/displayGroupsAdmin.js';
	import {
		MAX_PEN_LABEL_CHARS,
		MAX_PEN_UNIT_CHARS,
		axisText,
		bitBandHeightOptions,
		limitPlaceholder,
		type PenNumberField,
		type PenNumberTexts,
		type TagThresholds
	} from './penStyleEditor.js';

	interface Props {
		/** 0 始まりのペンの位置（欄の名前「ペン n の…」に使う）。 */
		index: number;
		pen: PenStyle;
		/** 数の欄の入力の文字（キーが無い欄は `pen` の値を出す）。 */
		texts?: PenNumberTexts;
		/** ペンのタグ（名前・単位の placeholder と bit の判定）。見つからなければ `null`。 */
		tag: { name: string; unit: string | null; dataType: string } | null | undefined;
		/** しきい値を引くタグの ID（未登録のタグは `null`）。 */
		thresholdTagId: number | null;
		/** タグのしきい値（まだ読んでいない・読めなかった = `null`）。 */
		thresholds: ReadonlyMap<number, TagThresholds> | null;
		thresholdsFailed: boolean;
		disabled?: boolean;
		/** 「保存されますが、効くのはトレンドだけです。」を出す（表示グループのトレンド以外）。 */
		trendOnlyNote?: boolean;
		/** その行の誤り（欄の後ろに出す）。 */
		errors?: readonly string[];
		onchange: (patch: Partial<PenStyle>) => void;
		ontext: (field: PenNumberField, text: string) => void;
	}

	let {
		index,
		pen,
		texts,
		tag,
		thresholdTagId,
		thresholds,
		thresholdsFailed,
		disabled = false,
		trendOnlyNote = false,
		errors = [],
		onchange,
		ontext
	}: Props = $props();

	/** 管理線の入力の欄（HH → LL の並び）。placeholder にはタグのしきい値を出す。 */
	const LIMIT_FIELDS = [
		['limitHh', 'HH', 'thresholdHh'],
		['limitH', 'H', 'thresholdH'],
		['limitL', 'L', 'thresholdL'],
		['limitLl', 'LL', 'thresholdLl']
	] as const;
	const LINE_STYLE_OPTIONS = [
		['solid', '実線'],
		['dashed', '破線'],
		['dashDot', '一点鎖線'],
		['dashDotDot', '二点鎖線'],
		['none', '線なし']
	] as const;
	const MARKER_OPTIONS = [
		['none', 'なし'],
		['circle', '丸'],
		['square', '四角'],
		['triangle', '三角'],
		['diamond', 'ひし形']
	] as const;
	const LINE_WIDTH_VALUES = [1, 1.5, 2, 2.5, 3, 3.5, 4];
	const BIT_STYLE_OPTIONS = [
		['step', '段差'],
		['band', '帯']
	] as const;

	const withTexts = $derived({ ...pen, texts });
	const noVisibleLine = $derived(
		pen.lineStyle === 'none' && (pen.marker === null || pen.marker === 'none')
	);

	function setText(key: 'label' | 'unit', value: string): void {
		onchange({ [key]: value });
	}

	/** 属性の select 用。空 = 既定（`null`）。 */
	function setChoice<K extends 'lineStyle' | 'marker' | 'axis' | 'bitStyle'>(
		key: K,
		value: string
	): void {
		onchange({ [key]: (value === '' ? null : value) as PenStyle[K] });
	}

	function setNumber(
		key: 'decimals' | 'lineWidth' | 'markerSize' | 'bitBandHeight',
		value: string
	): void {
		onchange({ [key]: value === '' ? null : Number(value) });
	}
</script>

<fieldset class="editor" {disabled} data-testid="pen-trend-style">
	<h4 class="sub-heading">トレンドの表示</h4>
	{#if trendOnlyNote}
		<p class="note">保存されますが、効くのはトレンドだけです。</p>
	{/if}
	{#if tag?.dataType === 'bit'}
		<p class="note">bit のタグは 0〜1 固定で、小数の桁・最小・最大は使いません。</p>
	{:else}
		<p class="note">「bit の表し」「帯の太さ」は bit のタグだけに効きます。</p>
	{/if}
	<label class="field">
		<span>ペン {index + 1} の凡例の名前</span>
		<input
			type="text"
			value={pen.label ?? ''}
			placeholder={tag?.name ?? ''}
			oninput={(e) => setText('label', e.currentTarget.value)}
		/>
		<span class="meta">{MAX_PEN_LABEL_CHARS} 文字まで。空はタグの名前。</span>
	</label>
	<label class="field">
		<span>ペン {index + 1} の単位</span>
		<input
			type="text"
			value={pen.unit ?? ''}
			placeholder={tag?.unit ?? ''}
			oninput={(e) => setText('unit', e.currentTarget.value)}
		/>
		<span class="meta">{MAX_PEN_UNIT_CHARS} 文字まで。空はタグの単位。</span>
	</label>
	<label class="field">
		<span>ペン {index + 1} の小数の桁</span>
		<select
			value={pen.decimals === null ? '' : String(pen.decimals)}
			onchange={(e) => setNumber('decimals', e.currentTarget.value)}
		>
			<option value="">既定（タグの設定）</option>
			{#each [0, 1, 2, 3, 4, 5, 6] as d (d)}
				<option value={String(d)}>{d}</option>
			{/each}
		</select>
	</label>
	<label class="field">
		<span>ペン {index + 1} の線種</span>
		<select
			value={pen.lineStyle ?? ''}
			onchange={(e) => setChoice('lineStyle', e.currentTarget.value)}
		>
			<option value="">既定（実線）</option>
			{#each LINE_STYLE_OPTIONS as [value, label] (value)}
				<option {value}>{label}</option>
			{/each}
		</select>
	</label>
	<label class="field">
		<span>ペン {index + 1} の線幅</span>
		<select
			value={pen.lineWidth === null ? '' : String(pen.lineWidth)}
			onchange={(e) => setNumber('lineWidth', e.currentTarget.value)}
		>
			<option value="">既定（{DEFAULT_LINE_WIDTH} px）</option>
			{#each LINE_WIDTH_VALUES as w (w)}
				<option value={String(w)}>{w} px</option>
			{/each}
		</select>
	</label>
	<label class="field">
		<span>ペン {index + 1} の bit の表し</span>
		<select
			value={pen.bitStyle ?? ''}
			onchange={(e) => setChoice('bitStyle', e.currentTarget.value)}
		>
			<option value="">既定（段差）</option>
			{#each BIT_STYLE_OPTIONS as [value, label] (value)}
				<option {value}>{label}</option>
			{/each}
		</select>
	</label>
	<label class="field">
		<span>ペン {index + 1} の帯の太さ</span>
		<select
			value={pen.bitBandHeight === null ? '' : String(pen.bitBandHeight)}
			disabled={pen.bitStyle !== 'band'}
			onchange={(e) => setNumber('bitBandHeight', e.currentTarget.value)}
		>
			<option value="">既定（{DEFAULT_BIT_BAND_HEIGHT} px）</option>
			{#each bitBandHeightOptions(pen.bitBandHeight) as h (h)}
				<option value={String(h)}>{h} px</option>
			{/each}
		</select>
	</label>
	<label class="field">
		<span>ペン {index + 1} のマーカー</span>
		<select value={pen.marker ?? ''} onchange={(e) => setChoice('marker', e.currentTarget.value)}>
			<option value="">既定（なし）</option>
			{#each MARKER_OPTIONS as [value, label] (value)}
				<option {value}>{label}</option>
			{/each}
		</select>
	</label>
	<label class="field">
		<span>ペン {index + 1} のマーカーの大きさ</span>
		<select
			value={pen.markerSize === null ? '' : String(pen.markerSize)}
			disabled={pen.marker === null || pen.marker === 'none'}
			onchange={(e) => setNumber('markerSize', e.currentTarget.value)}
		>
			<option value="">既定（{DEFAULT_MARKER_SIZE} px）</option>
			{#each [2, 3, 4, 5, 6, 7, 8] as m (m)}
				<option value={String(m)}>{m} px</option>
			{/each}
		</select>
	</label>
	{#if noVisibleLine}
		<p class="note">線もマーカーも無いと、線が見えません。</p>
	{/if}
	<label class="field">
		<span>ペン {index + 1} の縦軸</span>
		<select value={pen.axis ?? ''} onchange={(e) => setChoice('axis', e.currentTarget.value)}>
			<option value="">既定（左）</option>
			<option value="left">左</option>
			<option value="right">右</option>
		</select>
	</label>
	<label class="field">
		<span>ペン {index + 1} の縦軸の最小</span>
		<input
			type="text"
			inputmode="decimal"
			value={axisText(withTexts, 'yMin')}
			oninput={(e) => ontext('yMin', e.currentTarget.value)}
		/>
	</label>
	<label class="field">
		<span>ペン {index + 1} の縦軸の最大</span>
		<input
			type="text"
			inputmode="decimal"
			value={axisText(withTexts, 'yMax')}
			oninput={(e) => ontext('yMax', e.currentTarget.value)}
		/>
		<span class="meta">最小・最大が空の側は、タグの工学値レンジ → データの範囲。</span>
	</label>
	<h5 class="sub-heading">管理線</h5>
	<label class="check">
		<input
			type="checkbox"
			checked={pen.limitLines === true}
			onchange={(e) => onchange({ limitLines: e.currentTarget.checked ? true : null })}
		/>
		ペン {index + 1} の管理線を引く
	</label>
	{#each LIMIT_FIELDS as [field, label, tagKey] (field)}
		<label class="field">
			<span>ペン {index + 1} の管理線の {label}</span>
			<input
				type="text"
				inputmode="decimal"
				value={axisText(withTexts, field)}
				placeholder={limitPlaceholder(thresholdTagId, thresholds, thresholdsFailed, tagKey)}
				oninput={(e) => ontext(field, e.currentTarget.value)}
			/>
		</label>
	{/each}
	<p class="note">
		空はタグのしきい値。上書きした値は表示だけに使い、しきい値のイベントは変えません。チェックを外していても値は保存されます。
	</p>
	{#if tag?.dataType === 'bit'}
		<p class="note">bit のタグには管理線を引きません。</p>
	{/if}
</fieldset>
{#each errors as message, i (i)}
	<p class="field-error">{message}</p>
{/each}

<style>
	/* 表示グループのページの右ペインと同じ見た目（groups/+page.svelte の同名のクラス）。 */
	.editor {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		border: none;
		margin: 0;
		padding: 0;
	}

	.note {
		margin: 0;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		font-size: 0.85rem;
	}

	.field input,
	.field select {
		height: var(--banto-control-height);
		box-sizing: border-box;
		padding: 0 0.5rem;
		background: var(--banto-bg);
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius-md);
		color: inherit;
		max-width: 24rem;
	}

	.field .meta {
		color: var(--banto-text-muted);
		font-size: 0.75rem;
	}

	.field-error {
		margin: 0;
		color: var(--banto-danger);
		font-size: 0.8rem;
	}

	.sub-heading {
		margin: 0.5rem 0 0;
		font-size: 0.9rem;
	}

	.check {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		font-size: 0.85rem;
	}
</style>

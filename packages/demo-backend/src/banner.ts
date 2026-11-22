/**
 * 画面の上にデモであることを示す帯（設計 §3.4）。Svelte の部品にしないのは、製品のビルドに
 * デモのコードを入れないため（`setup.ts` の動的 import の先からだけ呼ぶ）。
 * 画面の配置（100vh の殻）を押し下げないよう、上端の中央に重ねて、クリックは下へ通す。
 */
export const DEMO_BANNER_ID = 'banto-industrial-demo-banner';

export function mountDemoBanner(text: string, doc: Document = document): HTMLElement {
	const existing = doc.getElementById(DEMO_BANNER_ID);
	if (existing) return existing;
	const banner = doc.createElement('div');
	banner.id = DEMO_BANNER_ID;
	banner.setAttribute('role', 'note');
	banner.textContent = text;
	Object.assign(banner.style, {
		position: 'fixed',
		top: '0',
		left: '50%',
		transform: 'translateX(-50%)',
		zIndex: '2147483647',
		padding: '0.125rem 0.75rem',
		borderRadius: '0 0 0.375rem 0.375rem',
		background: '#b45309',
		color: '#ffffff',
		font: '600 12px/1.5 system-ui, sans-serif',
		pointerEvents: 'none',
		whiteSpace: 'nowrap'
	} satisfies Partial<CSSStyleDeclaration>);
	doc.body.append(banner);
	return banner;
}

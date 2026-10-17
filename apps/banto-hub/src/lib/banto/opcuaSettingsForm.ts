import type { OpcUaSettings, OpcUaStatus } from './opcuaSettingsAdmin';

/** 状態のピルの表示（#634）。`tone` は `ok`・`bad`・中立（`null`）。 */
export function opcUaStatusPill(status: OpcUaStatus | null): {
	label: string;
	tone: 'ok' | 'bad' | null;
} {
	switch (status?.state) {
		case 'running':
			return { label: '待受中', tone: 'ok' };
		case 'starting':
			return { label: '起動中…', tone: null };
		case 'failed':
			return { label: '起動失敗', tone: 'bad' };
		case 'stopped':
			return { label: '停止', tone: 'bad' };
		default:
			return { label: '状態: 未確認', tone: 'bad' };
	}
}

function isIpv4(value: string): boolean {
	const octets = value.split('.');
	return (
		octets.length === 4 &&
		octets.every((octet) => (/^(0|[1-9]\d{0,2})$/.test(octet) ? Number(octet) <= 255 : false))
	);
}

function isIpv6(value: string): boolean {
	if (!value.includes(':')) return false;
	try {
		return new URL(`http://[${value}]/`).hostname.startsWith('[');
	} catch {
		return false;
	}
}

function isHost(value: string): boolean {
	if (isIpv4(value) || isIpv6(value)) return true;
	if (value.startsWith('[') && value.endsWith(']')) return isIpv6(value.slice(1, -1));
	const dns = value.endsWith('.') ? value.slice(0, -1) : value;
	if (dns.length === 0 || dns.length > 253) return false;
	return dns
		.split('.')
		.every((label) => /^(?=.{1,63}$)[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?$/.test(label));
}

function isLoopback(value: string): boolean {
	const address = value.startsWith('[') && value.endsWith(']') ? value.slice(1, -1) : value;
	return (
		(isIpv4(address) && address.startsWith('127.')) ||
		(isIpv6(address) && new URL(`http://[${address}]/`).hostname === '[::1]') ||
		address.toLowerCase() === 'localhost'
	);
}

function isUnspecified(value: string): boolean {
	return (
		value === '0.0.0.0' || (isIpv6(value) && new URL(`http://[${value}]/`).hostname === '[::]')
	);
}

export function isLanExposure(bind: string, advertisedHost: string): boolean {
	return (
		!isLoopback(bind.trim()) || (advertisedHost.trim() !== '' && !isLoopback(advertisedHost.trim()))
	);
}

/** UI で即座に案内するための検証。サーバー側でも同じ制約を検証する。 */
export function validateOpcUaSettings(input: OpcUaSettings): string | null {
	if (!isIpv4(input.bind) && !isIpv6(input.bind)) {
		return 'Bind アドレスには IP アドレスを入力してください';
	}
	if (!Number.isInteger(input.port) || input.port < 1 || input.port > 65535) {
		return 'ポートには 1～65535 の整数を入力してください';
	}
	if (!input.endpointPath.startsWith('/') || /[?#\s]/.test(input.endpointPath)) {
		return 'エンドポイントパスは / で始め、空白・?・# を含めないでください';
	}
	if (
		input.namespaceUri.trim() === '' ||
		input.namespaceUri !== input.namespaceUri.trim() ||
		['http://opcfoundation.org/UA/', 'urn:banto:opcua:server'].includes(input.namespaceUri)
	) {
		return '標準名前空間と異なる Namespace URI を入力してください';
	}
	if (input.advertisedHost !== null && !isHost(input.advertisedHost)) {
		return '公開ホストにはポートや URL を含めず、ホスト名または IP アドレスを入力してください';
	}
	if (['0.0.0.0', '::', '[::]'].includes(input.advertisedHost ?? '')) {
		return '公開ホストにはクライアントから接続できるホスト名または IP アドレスを入力してください';
	}
	if (isUnspecified(input.bind) && !input.advertisedHost) {
		return '全インターフェースで待ち受ける場合は公開ホストを入力してください';
	}
	return null;
}

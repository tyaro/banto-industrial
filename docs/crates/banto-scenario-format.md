# 時刻だけで値が決まる筋書きの形（版 1）

状態: **実装済み**（2026-11-26、banto-industrial#1335 で temp-scada#389 の形 版 1 を移した）。Rust の評価部は `crates/banto-scenario`、共通の試験ケースは `crates/banto-scenario/tests/`。TypeScript の評価部は当面 temp-scada の `apps/banto-scada/src/scenario/scenario.ts`。temp-scada では、製品のコード（`src/lib`）は使わず、GitHub Pages のデモ（[デモの案内](https://github.com/tyaro/temp-scada/blob/work/docs/guides/demo-pages.md)）だけが使う。banto-industrial では筋書きで動く模擬 PLC（#1335）が使う。

## 目的

同じ筋書きを、画面だけの模擬（ブラウザの中）と、サーバーの模擬（Rust）の両方で評価して、同じ時刻には同じ値を出す。値は時刻（UNIX 秒）だけで決まる。そのため、過去・未来のどの時刻の値も同じに作れ、現在値・警報・履歴が同じ筋書きを共有できる。

## 形

```json
{
	"format": "banto-scenario",
	"version": 1,
	"cycleSec": 180,
	"epochSec": 1767225600,
	"freezeAtSec": 70,
	"signals": {
		"Tank.Level": {
			"kind": "number",
			"points": [
				[0, 40],
				[60, 82],
				[180, 40]
			],
			"digits": 1
		},
		"Inflow": {
			"kind": "number",
			"base": 62,
			"waves": [{ "amplitude": 34, "periodSec": 180, "shiftSec": 5 }]
		},
		"Inflow2": { "like": "Inflow", "shiftSec": 30 },
		"Hours": {
			"kind": "number",
			"base": 4212,
			"rampPerHour": 1,
			"rampWrap": 1000
		},
		"Pump.Run": { "kind": "bit", "value": true },
		"Lamp": { "kind": "bit", "periodSec": 2, "onSec": 1 }
	},
	"events": [{ "atSec": 146, "durationSec": 22, "values": { "Pump.Run": false } }]
}
```

| 項目                | 意味                                                                                              |
| ------------------- | ------------------------------------------------------------------------------------------------- |
| `format`・`version` | `"banto-scenario"` と `1`。ほかは読まない                                                         |
| `cycleSec`          | 筋書きが一巡する秒（正の数）                                                                      |
| `epochSec`          | 時刻の基準（UNIX 秒、省くと 0）。周期の中の秒は `(t − epochSec)` を周期で割った余り。ランプの起点 |
| `freezeAtSec`       | 省略可。時刻を止める周期の中の秒（0 以上・周期未満）。止めた時刻は `epochSec + freezeAtSec`       |
| `signals`           | 信号の名前と定義。名前は空にしない                                                                |
| `events`            | 時刻つきの出来事。省略可                                                                          |

数値の信号（`kind: "number"`）の値は、次の順に足す。

1. `base`（省くと 0）
2. `waves` の各項: `amplitude · sin(2π · ((t − shiftSec_信号 − shiftSec_波) mod periodSec) / periodSec)`。sin は周期で余りを取ってから求めるので、大きな時刻でも言語の間で値がずれにくい
3. `points`（周期の中の秒と値の点列）を直線でつないだ値。周期の中の秒は `(t − shiftSec_信号 − epochSec) mod cycleSec`。点の秒は 0〜周期で、戻らない。同じ秒の点を続けると段差になる。最初の点より前は最初の値、最後の点より後は最後の値。点が無ければ 0
4. `rampPerHour · (t − epochSec) / 3600`。`rampWrap` が正なら、この分だけをその値で割った余りにする（運転時間・積算のような、周期で戻らない値）

`digits`（0〜9 の整数）があれば、最後に小数のその桁へ丸める（0.5 は 0 から遠い方）。桁へ寄せた中間の値（`|値| · 10^digits`）が有限でなければ、丸めずにそのまま返す（有限の値を丸めで無限にしない。`cases/huge.json`）。各項の和そのものが有限の範囲を超えたときの値（無限大）は決めず、筋書きを書く側が避ける（版 1 では読込で検出しない）。

ビットの信号（`kind: "bit"`）は、`periodSec` が 0（既定）なら固定の `value`（既定 false）。正なら `(t − shiftSec) mod periodSec < onSec` の間だけ ON。

`like` は別の信号の定義を写し、自分が書いた項目だけを置き換える（対象ごとの位相のずらし・上書き）。`like` は続けてたどれるが、循環は誤り。種類（`kind`）は元と同じでなければならない。

出来事は、周期の中の秒 `atSec`（0 以上・周期未満）から `durationSec`（正・周期以下）の間、`values` の信号を固定の値に置き換える。周期の終わりをまたいでもよい（`atSec + durationSec > cycleSec`）。重なる出来事は、配列で後のものが勝つ。値の種類は信号と同じ（数値の信号に数、ビットの信号に true・false）。出来事の値は丸めない。

知らない項目、範囲を外れた数、無い信号への参照、省略できる項目に書いた `null` は、読込の誤りにする（`null` は省略と同じに扱わない）。誤りは TypeScript・Rust とも `ScenarioError`。信号の名前は `__proto__`・`toString` のような名前でもほかと同じに扱う（TypeScript は原型を持たない辞書に入れる。`names.json` が確かめる）。

## 言語の間の一致

- 余りは `r = x % d; if r < 0 { r += d }` で、両方で同じ手順を取る。丸めは `round(|x| · 10^digits) / 10^digits` に符号を戻す（10 の累乗は表で引く）。
- sin は言語の数学ライブラリの差で最後の桁がずれうる。共通の試験ケースは 1e-9 までの差を許す。丸めの境目のちょうど上で sin がずれると、丸めた値が 1 桁ずれる可能性は残る（試験ケースの時刻では起きていない）。
- 試験ケース（`crates/banto-scenario/tests/cases/*.json`）は、筋書き（`scenario`、または `scenarioFile` で別のファイル）・`frozenTimeSec`・時刻ごとの期待値を持つ。`basics.json` は手で計算した値。`water.json` は Pages のデモの筋書きを TypeScript の評価部で書いた値で、筋書きを変えたら（temp-scada で）`SCENARIO_BLESS=1 pnpm --filter banto-scada exec vitest run src/scenario` で書き直し、Rust の試験（`cargo test -p banto-scenario`）が同じ値を出すことを確かめる。誤りの例は `tests/invalid.json`。water.json を書き直したら、ここの `tests/cases/water.json` と `tests/scenarios/water.scenario.json` も temp-scada から写す。

## 決めていないこと

- 通信の途切れ（`hang`）は筋書きの形に入れず、模擬 PLC の設定に書く（2026-11-26 オーナー決定、#1335）。
- 信号から別の信号を計算する式（例: 液位が 80 を越えたら ON）。版 1 では、しきい値を越える時刻を出来事に書き下す。

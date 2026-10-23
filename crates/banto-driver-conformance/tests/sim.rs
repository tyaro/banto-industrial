//! 値の層のシミュレータ（`banto_driver_engine::sim`）を、セッション型の適合テストの
//! ワイヤに依らない項目（C3 の順序・空の要求、C4 の `Rejected` の形、C8 の書いて読み戻す）
//! で確かめる（docs/banto-hub-driver-sim-design.md §3.5）。
//!
//! `session::run_all_against` は TCP の相手（`FaultProxy`）を前提にし、公開 API を
//! 変えずにシミュレータへ向けられないので、同じ性質の主張をここに書く。

use banto_driver_api::{
    DataType, DriverAddress, DriverId, ItemError, StringEncoding, TagKind, TagSpec, Value,
    WriteItem,
};
use banto_driver_engine::sim::SimDevice;

fn spec(canonical: &str, writable: bool, kind: TagKind) -> TagSpec {
    let words = kind.word_count();
    TagSpec::new(
        DriverAddress::new(
            DriverId::new("sim").unwrap(),
            canonical,
            "D",
            writable,
            words,
        ),
        kind,
    )
}

fn kinds() -> Vec<TagKind> {
    vec![
        TagKind::numeric(DataType::Bit),
        TagKind::numeric(DataType::I16),
        TagKind::numeric(DataType::U16),
        TagKind::numeric(DataType::I32),
        TagKind::numeric(DataType::U32),
        TagKind::numeric(DataType::F32),
        TagKind::numeric(DataType::I64),
        TagKind::numeric(DataType::U64),
        TagKind::numeric(DataType::F64),
        TagKind::string(4, StringEncoding::Utf8),
    ]
}

/// C3: 結果は spec と同じ数・同じ順（単独・一括・逆順・同じ spec を 2 回）、空は空。
#[tokio::test]
async fn c3_one_result_per_spec_in_order() {
    let dev = SimDevice::new();
    let mut s = dev.connect();
    assert!(s.read_batch(&[]).await.unwrap().is_empty());
    let specs: Vec<TagSpec> = kinds()
        .into_iter()
        .enumerate()
        .map(|(i, k)| spec(&format!("D{}", i * 10), true, k))
        .collect();
    // 固定の値を置き、順序を確かめられるようにする。
    for (i, sp) in specs.iter().enumerate() {
        dev.set(
            &sp.address.canonical,
            match sp.kind {
                TagKind::Numeric(DataType::Bit) => Value::Bit(true),
                TagKind::Numeric(_) => Value::F64(i as f64),
                _ => Value::Str(format!("s{i}")),
            },
        );
    }
    let all = s.read_batch(&specs).await.unwrap();
    assert_eq!(all.len(), specs.len());
    for (i, sp) in specs.iter().enumerate() {
        let one = s.read_batch(std::slice::from_ref(sp)).await.unwrap();
        assert_eq!(one, vec![all[i].clone()], "{i}");
    }
    let rev: Vec<TagSpec> = specs.iter().rev().cloned().collect();
    let mut expect = all.clone();
    expect.reverse();
    assert_eq!(s.read_batch(&rev).await.unwrap(), expect);
    let twice: Vec<TagSpec> = specs.iter().flat_map(|s| [s.clone(), s.clone()]).collect();
    let r = s.read_batch(&twice).await.unwrap();
    assert_eq!(r.len(), twice.len());
    for (i, pair) in r.chunks(2).enumerate() {
        assert_eq!(pair[0], all[i]);
        assert_eq!(pair[1], all[i]);
    }
}

/// 生成値も、どの種類でも `Ok` で、種類に合った形（文字列は語数 × 2 バイト以内）。
#[tokio::test]
async fn generated_values_have_the_shape_of_their_kind() {
    let dev = SimDevice::new();
    let mut s = dev.connect();
    for (i, k) in kinds().into_iter().enumerate() {
        let sp = spec(&format!("D{i}"), true, k.clone());
        let v = s.read_batch(&[sp]).await.unwrap().remove(0).unwrap();
        match (&k, v) {
            (TagKind::Numeric(DataType::Bit), Value::Bit(_)) => {}
            (TagKind::Numeric(_), Value::F64(x)) => assert!(x.is_finite() && x >= 0.0),
            (TagKind::Str { words, .. }, Value::Str(t)) => assert!(t.len() <= *words as usize * 2),
            (k, v) => panic!("{k:?} -> {v:?}"),
        }
    }
}

/// C4: 書けない領域・種類違い・範囲外は、その項目だけ `Rejected` で、
/// 同じバッチの他の項目と接続は無事。
#[tokio::test]
async fn c4_rejections_are_item_level_and_keep_the_session() {
    let dev = SimDevice::new();
    let mut s = dev.connect();
    let ro = spec("X1", false, TagKind::numeric(DataType::U16));
    let ok = spec("D1", true, TagKind::numeric(DataType::U16));
    let r = s
        .write_batch(&[
            WriteItem::new(ro, Value::F64(1.0)),
            WriteItem::new(ok.clone(), Value::Bit(true)),
            WriteItem::new(ok.clone(), Value::F64(70000.0)),
            WriteItem::new(ok.clone(), Value::F64(5.0)),
        ])
        .await
        .unwrap();
    assert!(matches!(r[0], Err(ItemError::Rejected(_))));
    assert!(matches!(r[1], Err(ItemError::Rejected(_))));
    assert!(matches!(r[2], Err(ItemError::Rejected(_))));
    assert_eq!(r[3], Ok(()));
    assert_eq!(
        s.read_batch(&[ok]).await.unwrap(),
        vec![Ok(Value::F64(5.0))]
    );
}

/// C8: 書いて読み戻す往復を 100 回繰り返しても、書いた値のまま（同じセッション）。
/// 張り直したセッションでも同じ。
#[tokio::test]
async fn c8_alternating_reads_and_writes_read_back_as_written() {
    let dev = SimDevice::new();
    let mut s = dev.connect();
    let u = spec("D1", true, TagKind::numeric(DataType::U16));
    let t = spec("D5", true, TagKind::string(2, StringEncoding::Utf8));
    for i in 0..100u32 {
        let (v, text) = (
            Value::F64(f64::from(i)),
            Value::Str(format!("t{}", i % 100)),
        );
        let r = s
            .write_batch(&[
                WriteItem::new(u.clone(), v.clone()),
                WriteItem::new(t.clone(), text.clone()),
            ])
            .await
            .unwrap();
        assert!(r.iter().all(|x| x.is_ok()));
        let got = s.read_batch(&[u.clone(), t.clone()]).await.unwrap();
        assert_eq!(got, vec![Ok(v), Ok(text)], "{i}");
    }
    assert_eq!(dev.write_count(), 200);
    let mut s2 = dev.connect();
    assert_eq!(
        s2.read_batch(std::slice::from_ref(&u)).await.unwrap(),
        vec![Ok(Value::F64(99.0))]
    );
}

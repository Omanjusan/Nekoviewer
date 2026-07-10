# 画像の向き（Exif自動回転・手動回転・ON/OFF設定）

サムネイル・ビューアーで画像の向きをどう決めるかについての一群の機能。3つの層で構成される。

1. Exif Orientation自動回転（デコード時の焼き込み）
2. 手動回転（ビューアー操作、レンダリング時の追加回転）
3. Exif自動回転のON/OFF設定（1の適用有無を切り替える）

## 1. Exif Orientation自動回転

対応フォーマット: JPEG/PNG/TIFF/WebP/AVIF全対応。サムネイル・ビューアー双方に自動反映。

- JPEG/PNG/TIFF等、image crateネイティブ対応フォーマットは`decode_image_bytes`
  （[decode.rs](../../src/fs/archive/decode.rs)）でExif Orientationを検出・適用
- WebP: RIFFチャンクを自前パースしてEXIFチャンクのOrientationを検出（`webp_exif_orientation`）。
  静止画・アニメフォールバック双方の`decode_image_bytes`分岐、および`RingAnimation::from_source`
  （[cache.rs](../../src/cache.rs)、静止画確定時のみ適用）に反映
- AVIF: `libavif`(安全ラッパー)にはexif/irot/imirへのアクセスが無いため、`decode_avif`は生FFI
  （`libavif-sys`、[anim.rs](../../src/anim.rs)の`AvifSeqState`と同系統）を使用。コンテナ自体が持つ
  `irot`(回転)/`imir`(ミラー)変換を優先し、無ければ埋め込みExifタグにフォールバックする
  `avif_container_orientation`が`Orientation`列挙体に正規化し、`decode_avif`とアニメ単一フレーム
  確定時の両方から共有する

いずれもデコード直後にピクセルへ焼き込む方式（`apply_orientation`）。以降の呼び出し元は
回転後の寸法をそのまま使えばよく、レンダリング側で個別の回転対応は不要。

## 2. 手動回転

ビューアーでユーザーが任意にページを90/180/270度回転できる操作。Exif自動回転とは独立
（Exifは既にデコード時にピクセルへ焼き込み済みのため、手動回転は単純加算のみで済む）。

- 回転は時計回り・逆時計回りボタン（top bar/フルスクリーンソートバー）で操作。Exif誤タグや
  自動回転未対応フォーマットに当たった際の逃げ道にもなる
- シングルページ: fit(枠合わせ)表示・zoom_actual(原寸+スクロール)表示の両方で回転対応
- 見開き表示: 2ページを個別回転せず、外接矩形(footprint)を1つの剛体としてcontain-fit+
  中心回転（`paint_spread_rotated`、[view_reader.rs](../../src/view_reader.rs)）
- ページ送り・ソート変更時は回転角度を自動リセット。「角度引き継ぎ」トグルON時は全ページ共通の
  角度をセッション中（非永続）維持
- 状態管理・純粋関数は[rotation.rs](../../src/rotation.rs)に分離（`RotationState`、
  `normalize_360`等）、TextureHandle非依存でテスト可能

## 3. Exif Orientation ON/OFF設定

スキャン・加工ツール経由の漫画アーカイブでは誤ったOrientationタグが埋め込まれるケースがあるため、
自動回転(1)の適用有無を設定値で切り替えられるようにしたグローバル設定（手動回転(2)は個別ページの
補正用途）。

- 方式は「再デコードでOrientation適用をスキップ」。`ViewerConfig::exif_orientation_enabled`
  （永続設定）を`target_size`と同型で`LoadRequest`からデコード層末端まで貫通させる
- サムネイルはビューアーの設定と独立、常時EXIF自動回転ON固定（サムネDBキャッシュが
  filename+mtimeキーのみでOrientation適用有無を区別できないため、スコープ外とする判断）
- 設定ダイアログのViewerタブ、およびビューアーツールバー（top bar/フルスクリーンソートバー、
  回転引き継ぎチェックボックスの隣）の両方から切替可能。どちらの経路で変更されても
  `poll_resize_redecode`と同じ「毎フレーム値比較」方式（`exif_orientation_enabled_last_seen`）で
  検知し、開いているアーカイブのPageCache全破棄＋ビューアー側の全ページテクスチャ破棄を即時発火。
  以降は通常の毎フレームフロー（`prefetch_pages`/`update_textures`）が再デコード・再アップロードを拾う
- 手動回転(2)との整合: OFF→ON=手動回転をEXIF値へ強制リセット、ON→OFF=EXIF回転角度を手動角度へ
  加算補正して見た目を維持（`RotationState::on_exif_enabled`/`on_exif_disabled`）。見開き時の
  EXIF基準ページは「仮想ページでない方を優先→両方実ページならインデックスが若い方」
- 既知の制限: AVIFのみOrientation単体検出の軽量パスが無く（フルデコード相当のFFIコストを
  避けるため）ON→OFF切替時に常にNoTransforms扱い。該当ページがirot/imir/exifを実際に持つ場合の
  みON→OFF切替の瞬間90度単位で一瞬ズレることがある（発生頻度は低い想定、既知の割り切り）

## テスト

- rotation.rs: 単体テスト（回転角度の正規化、carry_over分岐、on_exif_enabled/on_exif_disabled等）
- view_reader.rs: 見開き回転の幾何計算（`spread_rotation_fit`、TextureHandle非依存の純粋関数）
- decode.rs: Exif検出（JPEG/WebP/AVIF irot・imir・フォールバック）、
  `detect_orientation_for_toggle`のフォーマット別挙動・AVIF既知制限

## 未使用のまま温存しているコード

`rotation.rs`の`resolve_base_orientation`/`effective_angle`/`PageOrientationRef`。
ON→OFF切替の見た目維持補正は「基準ページを1つ選んでからそのページだけOrientation検出する」
軽量な設計にしたため、両ページの向きを先に揃える`resolve_base_orientation`は不要だった。
将来レンダリング時逆回転方式に切り替える判断をした場合の用途として残している。

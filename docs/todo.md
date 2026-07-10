(A) Exif Orientation対応 — 完了（JPEG/PNG/TIFF/WebP/AVIF全対応）
    - v1.3.0でJPEG/PNG/TIFF等image crateネイティブ対応フォーマットのExif Orientation検出・適用は
      実装済み（decode_image_bytes、サムネ・ビューアー双方に自動反映）。
    - WebP: 実装済み（37fbb55）。RIFFチャンクを自前パースしてEXIFチャンクのOrientationを検出
      （decode.rs `webp_exif_orientation`）。静止画・アニメフォールバック双方のdecode_image_bytes分岐、
      および`RingAnimation::from_source`（cache.rs、静止画確定時のみ適用）に反映済み。
    - AVIF: 実装済み。`libavif`(安全ラッパー)にはexif/irot/imirへのアクセスが無いため、
      `decode_avif`（decode.rs）を生FFI（`libavif-sys`、`anim.rs`の`AvifSeqState`と同系統）に置き換え。
      コンテナ自体が持つ`irot`(回転)/`imir`(ミラー)変換を優先し、無ければ埋め込みExifタグにフォールバック
      する`avif_container_orientation`を新設（`Orientation`列挙体に正規化、`decode_avif`と
      アニメ単一フレーム確定時の両方から共有）。未使用になった`libavif`クレート依存はCargo.tomlから削除。
      テストはlibavif-sys生FFIでの自作AVIFエンコード（irot/imir/Exifそれぞれ）で検証。
(B) 手動回転機能 — 完了（feat/image-rotationブランチ、未コミット）
    - ビューアーでユーザーが任意にページを90/180/270度回転できる操作。Exif自動回転(A)とは独立
      （EXIFは既にデコード時にピクセルへ焼き込み済みのため、手動回転は単純加算のみで済む）。
    - 回転は時計回り・逆時計回りボタン（top bar/フルスクリーンソートバー）で操作。
      Exif誤タグや(A)未対応フォーマットに当たった際の逃げ道にもなる。
    - シングルページ: fit(枠合わせ)表示・zoom_actual(原寸+スクロール)表示の両方で回転対応。
    - 見開き表示: 2ページを個別回転せず、外接矩形(footprint)を1つの剛体としてcontain-fit+
      中心回転（`paint_spread_rotated`）。EXIF基準点判定は不要と判断しスキップ。
    - ページ送り・ソート変更時は回転角度を自動リセット。「角度引き継ぎ」トグルON時は
      全ページ共通の角度をセッション中（非永続）維持。
    - 単体テスト: rotation.rs 10件（carry_over分岐含む）、view_reader.rs 3件
      （見開き回転の幾何計算、TextureHandle非依存の純粋関数として分離）。cargo test 90件 all green。
    - 未使用のまま温存: `orientation_rotation_degrees`/`resolve_base_orientation`/`effective_angle`
      （項目(D)実装時にEXIF基準角度の再計算で使う想定、削除しない）。
(C) 拡縮
    - ズーム・パン。手動回転(B)と合わせてページの見た目調整の一群として扱う。
(D) Exif Orientation ON/OFF設定
    - スキャン・加工ツール経由の漫画アーカイブでは誤ったOrientationタグが埋め込まれるケースがあるため、
      自動回転の適用有無を設定値で切り替えられるようにする。(B)の手動回転が先にあれば個別補正はできるため、
      本項目はグローバルな一括対応の位置づけ。
    - 着手前の要検討事項: EXIF自動回転(A)は既にデコード時にピクセルへ焼き込み済みのため、OFF化には
      「再デコードでOrientation適用をスキップする」か「レンダリング時に逆回転を掛ける」のどちらかの
      方式選定が必要（詳細はauto-memory `feat-image-rotation-handoff` 参照）。
(E) 見開きボタンをビューアーウィンドウに移動
(F) コマフォーカス移動
(G) OCR-翻訳接続
(H) コピペ（当アプリからのコピーだけでいい書き込みをみる系はやらない。エクスプローラー代替にはならない）OS側のエクスプローラーを呼ぶのはあってもいいかも
(I) キーボードオペレーション対応をエクスプローラー部でも行う(詳細、メニューアクセスとか)

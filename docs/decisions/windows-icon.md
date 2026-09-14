<!-- markdownlint-disable -->
# アプリアイコン搭載（Windows exe / 実行中ウィンドウ）（決定記録）

flatpak版はデスクトップ統合(`.desktop`+icon)でアイコン搭載済みだったが、AppImage/Linuxネイティブ/Windowsの3配布形態は未搭載だった状態からの対応。今回のスコープはユーザー数の多いWindows向けに縮小し、他デプロイ形態（AppImage/Linuxネイティブ）は着手時にこの記録を流用する前提で残す。

## 現状把握で分かったこと

- **flatpak**: `packaging/flatpak/*.desktop` + 512x512 png。ローカルインストール(user)だと実体は
  `~/.local/share/flatpak/app/<app-id>/x86_64/master/<commit>/files/share/icons/hicolor/512x512/apps/*.png`、
  XDGアイコンテーマ検索パスに乗るのは`~/.local/share/flatpak/exports/share/icons/...`のシンボリックリンク側。
  「exportsディレクトリがXDGアイコンテーマパスに登録される」のがflatpakのアイコン搭載の正体。
- **AppImage**: `packaging/appimage/`に同じ`.desktop`+512x512 pngが既にあり、`build-appimage.sh`が
  AppDir直下にコピーしている。appimagetoolの仕様上、Icon名と同名pngがAppDir直下にあれば`.DirIcon`が
  自動生成されるため、ファイルマネージャでのサムネ表示は**既にできている可能性が高い（未検証）**。
- **Windows**: `.ico`埋め込み用の`build.rs`が存在せず、exeは完全にノーアイコンだった。
- **実行中ウィンドウのアイコン**（タイトルバー/タスクバー/Alt-Tab）は、winit使用中にもかかわらず
  `with_window_icon`系の呼び出しがどこにも無く、**flatpak版含め全プラットフォーム共通で未設定**だった。
  これはexe埋め込みとは別レイヤーの話（exeアイコンは未起動時の見た目、window iconは実行中の見た目）。

## スコープ判断

- Linuxネイティブ単体バイナリ配布でのランチャーメニュー統合（`.desktop`+iconをXDGパスにインストール）は
  今回スコープ外にした。理由は`$XDG_BIN_HOME`か`~/.local/bin`か、update-desktop-databaseの呼び出し、
  アンインストール手順など、AppImage向けmuslビルドのXDGマイグレーション（別件、設計確定・実装未着手、
  [feat_manga_ocr_translate_handoff.md]系ではなく appimage_musl_xdg_migration メモ側）と設計判断が
  丸かぶりするため。ここで先に決めてしまうと後で手戻りになる。
- 一方、実行中ウィンドウアイコン(winit)は配布形態に関係なくバイナリ自身が持つ機能なので、
  Linuxネイティブ版にもタダで効く。「ランチャー登録」だけが今回の宿題として残る。

## 実装内容

- `packaging/windows/nekoviewer.ico`: 既存アプリアイコン(`packaging/appimage/*.png`, 512x512)から
  ImageMagick(`convert`)で256/128/64/48/32/16の6解像度を1つのicoにまとめて生成。
- `build.rs` + `embed-resource`(build-dependency, windows限定ではなくcrate自体が非Windowsでno-op):
  `packaging/windows/nekoviewer.rc`（`IDI_ICON1 ICON "nekoviewer.ico"`）をコンパイルしてexeにリソース
  埋め込み。エクスプローラー/タスクバーピン留め/ショートカットのアイコン表示用。
- `src/winit_app.rs`: `app_icon()`ヘルパーを追加。`packaging/appimage/*.png`を`include_bytes!`で埋め込み、
  `image`crateで64x64にリサイズ・デコードして`winit::window::Icon`を生成（`OnceLock`でキャッシュ、以降は
  clone）。explorer/viewer/status(debugのみ)/translateの4窓すべての`Window::default_attributes()`に
  `.with_window_icon(app_icon())`を追加。実行中のタイトルバー/タスクバー/Alt-Tab表示用。

## 検証

- Linux release buildで`cargo build --release`が既存warning以外エラーなく通ることを確認
  （build.rsが非Windowsでno-opであることの確認を兼ねる）。
- Windows版は`test-release.yml`（workflow_dispatch、Windowsのみ選択、`feat/windows-icon`ブランチ）で
  CI実行。`embed-resource`によるリソースコンパイル込みでビルド成功（9m20s、CIログ上エラー無し）。
  draft prerelease `test-20260911-0612` が作成済み。実機でのexeアイコン・実行中タスクバーアイコンの
  目視確認はユーザー側で実施予定。

## 他デプロイ形態への流用メモ（着手時に読む）

- AppImageは`.DirIcon`自動生成の実挙動を先に検証（appimagetoolのログ/生成物を確認するだけ、コスト極小）。
  ダメだった場合のみ`build-appimage.sh`に`.DirIcon`の明示コピー/シンボリックリンクを1行追加。
- Linuxネイティブのランチャー統合は、上記の通りXDGマイグレーション作業とまとめて設計する。
  `install.sh`的なスクリプトで`.desktop`+iconを`~/.local/share/applications/`
  `~/.local/share/icons/hicolor/512x512/apps/`に配置する形を想定（詳細は未確定）。
- 実行中ウィンドウアイコン(winit `app_icon()`)は既に全プラットフォーム共通で対応済みなので、
  AppImage/Linuxネイティブ側で追加実装は不要。

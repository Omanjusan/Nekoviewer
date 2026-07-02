use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::gui_config::WindowSlot;
use crate::view_status::StatusData;

// ── viewer → controller 間メッセージ ───────────────────────────────────────

/// viewer から controller/app へ伝えるファイル間ナビゲーション要求
#[derive(Clone, Copy, PartialEq)]
pub enum ViewerNav {
    None,
    PrevFile,
    NextFile,
}

/// viewer.show() の戻り値。viewer → controller への通知をまとめて返す。
#[derive(Clone)]
pub struct ViewerOutput {
    pub nav: ViewerNav,
    pub close_requested: bool,
    /// Some(_) のとき app 側でスロットを永続化する
    pub save_slots: Option<[Option<WindowSlot>; 4]>,
}

// ── ステータス即時更新要求 ────────────────────────────────────────────────────

/// ステータスウィンドウのデータを次フレームで即時更新するよう要求する。
/// 呼び出し元 viewport が root でない場合は
/// `ctx.request_repaint_of(egui::ViewportId::ROOT)` も併せて発行すること。
pub fn request_status_update(flag: &Arc<AtomicBool>) {
    flag.store(true, Ordering::Relaxed);
}

// ── ステータスデータ更新 ──────────────────────────────────────────────────────

/// リリース/デバッグ共通フィールドを更新する
pub fn update_status_data(
    data: &mut StatusData,
    page_cache_used_bytes: usize,
    page_cache_max_bytes: usize,
    file_cache_used_bytes: usize,
    file_cache_max_bytes: usize,
) {
    data.page_cache_used_bytes = page_cache_used_bytes;
    data.page_cache_max_bytes  = page_cache_max_bytes;
    data.file_cache_used_bytes = file_cache_used_bytes;
    data.file_cache_max_bytes  = file_cache_max_bytes;
}

/// デバッグビルド専用フィールドを更新する
#[cfg(debug_assertions)]
pub fn update_status_data_debug(
    data: &mut StatusData,
    frame_dt_ms: f32,
    thumb_pending: usize,
    pending_loads: usize,
    thumbnails_loaded: usize,
    scan_state: &'static str,
) {
    data.frame_dt_ms      = frame_dt_ms;
    data.thumb_pending    = thumb_pending;
    data.pending_loads    = pending_loads;
    data.thumbnails_loaded = thumbnails_loaded;
    data.scan_state       = scan_state;
}

// ── ナビゲーション純粋ロジック ────────────────────────────────────────────

/// direction(+1/-1) 方向に from_idx から次の有効ファイルを探す。
///
/// 返り値: `Some((new_idx, path, is_raw_file))` または `None`（端まで到達）
/// 呼び出し元は `is_raw_file == true` なら `ViewerState::new_raw`、
/// `false` なら `ViewerState::new` を呼ぶこと。
/// `ViewerState::new` が None を返したパスは `newly_invalid` に追加して返す。
/// direction(+1/-1) 方向に from_idx から次の候補ファイルを返す。
///
/// 返り値: `Some((idx, path, is_raw_file))` または `None`（端まで到達）
/// - `is_raw_file == true` → 呼び出し元は `ViewerState::new_raw` を使うこと
/// - `is_raw_file == false` → 呼び出し元は `ViewerState::new` を試み、
///   None（画像なし）なら `mark_archive_invalid` してこの idx で再度呼ぶこと
pub fn find_next_file(
    archives: &[PathBuf],
    raw_image_files: &HashSet<PathBuf>,
    invalid_archives: &HashSet<PathBuf>,
    from_idx: usize,
    direction: i32,
) -> Option<(usize, PathBuf, bool)> {
    let total = archives.len() as i32;
    let mut idx = from_idx as i32 + direction;

    loop {
        if idx < 0 || idx >= total {
            return None;
        }
        let path = &archives[idx as usize];

        if raw_image_files.contains(path) {
            return Some((idx as usize, path.clone(), true));
        }
        if invalid_archives.contains(path) {
            idx += direction;
            continue;
        }
        return Some((idx as usize, path.clone(), false));
    }
}

// ── ビューアー既定スロット解決 ────────────────────────────────────────────

/// ビューアーを開くときの既定の位置・サイズスロットを解決する純粋関数。
///
/// 評価順:
/// - (a) `default_slot`（conf 由来の index 0..3）が `None` なら即フォールバック。
/// - (b) 該当スロットが空（`None`）ならフォールバック。
///
/// 両方を満たすときだけ `Some(WindowSlot)` を返す。`None` は「デフォルト無し」を意味する。
///
/// 注: (b) は呼び出しのたびに現在の `slots` で評価されるため、セッション途中に
///     保存したスロット値が、次にビューアーを開いたとき既定値として有効化される。
pub fn resolve_default_slot(
    default_slot: Option<usize>,
    slots: &[Option<WindowSlot>; 4],
) -> Option<WindowSlot> {
    let idx = default_slot?;             // (a) conf 値が空欄/不正なら None
    slots.get(idx).copied().flatten()    // (b) 該当スロット未保存なら None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(x: i32) -> WindowSlot {
        WindowSlot { x, y: 0, w: 800, h: 600 }
    }

    #[test]
    fn default_slot_none_when_unconfigured() {
        // (a) 失敗: conf 空欄/不正 → デフォルト無し
        let slots = [Some(slot(10)), None, None, None];
        assert!(resolve_default_slot(None, &slots).is_none());
    }

    #[test]
    fn default_slot_none_when_target_empty() {
        // (b) 失敗: 番号は正しいが該当スロット未保存 → デフォルト無し
        let slots = [None, None, None, None];
        assert!(resolve_default_slot(Some(1), &slots).is_none());
    }

    #[test]
    fn default_slot_resolved_when_present() {
        // (a)(b) 成立 → 該当スロットを返す
        let slots = [None, Some(slot(42)), None, None];
        let got = resolve_default_slot(Some(1), &slots).expect("resolved");
        assert_eq!(got.x, 42);
    }
}

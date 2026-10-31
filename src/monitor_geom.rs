//! モニター配置の幾何計算（ウィンドウ位置スロットの「モニター番号の逆算」と「画面外補正」用）。
//! 座標は egui の論理座標（物理px ÷ pixels_per_point）。スロットの x/y/w/h と同じ空間で扱う。
//! 純粋な計算だけを持ち、モニター一覧の取得は winit_app 側（`winit_app::monitor_rects`）が担う。

/// 1 枚のモニターの矩形（論理座標、左上起点）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// 画面外補正で、窓を最低でもこのpx分は対象モニターの中に残す。
const MIN_VISIBLE: i32 = 100;

impl MonitorRect {
    /// 物理座標のモニター矩形を論理座標へ直す。
    pub fn from_physical(x: i32, y: i32, w: u32, h: u32, ppp: f32) -> Self {
        let ppp = if ppp.is_finite() && ppp > 0.0 { ppp } else { 1.0 };
        Self {
            x: (x as f32 / ppp).round() as i32,
            y: (y as f32 / ppp).round() as i32,
            w: (w as f32 / ppp).round() as u32,
            h: (h as f32 / ppp).round() as u32,
        }
    }

    fn right(&self) -> i64 { self.x as i64 + self.w as i64 }
    fn bottom(&self) -> i64 { self.y as i64 + self.h as i64 }
}

/// 番号を安定させるため、左→右、同じ x なら上→下の順に並べる。
/// OS の列挙順はバラつきうるので、表示用の「モニターN」はこの順の N（1始まり）にする。
pub fn sort_for_numbering(monitors: &mut [MonitorRect]) {
    monitors.sort_by_key(|m| (m.x, m.y));
}

fn overlap_area(slot: (i32, i32, u32, u32), m: &MonitorRect) -> i64 {
    let (x, y, w, h) = slot;
    let ox = (x as i64 + w as i64).min(m.right()) - (x as i64).max(m.x as i64);
    let oy = (y as i64 + h as i64).min(m.bottom()) - (y as i64).max(m.y as i64);
    if ox > 0 && oy > 0 { ox * oy } else { 0 }
}

/// スロット（x,y,w,h）が最も多く重なるモニターの添字。どのモニターとも重ならなければ None。
pub fn monitor_index_for_slot(x: i32, y: i32, w: u32, h: u32, monitors: &[MonitorRect]) -> Option<usize> {
    monitors
        .iter()
        .enumerate()
        .map(|(i, m)| (i, overlap_area((x, y, w, h), m)))
        .filter(|&(_, a)| a > 0)
        .max_by_key(|&(_, a)| a)
        .map(|(i, _)| i)
}

/// 補正の基準にするモニター。重なり最大のものを選び、どれとも重ならなければ窓の中心に最も近いもの。
pub fn target_monitor(x: i32, y: i32, w: u32, h: u32, monitors: &[MonitorRect]) -> Option<&MonitorRect> {
    if let Some(i) = monitor_index_for_slot(x, y, w, h, monitors) {
        return monitors.get(i);
    }
    let cx = x as i64 + w as i64 / 2;
    let cy = y as i64 + h as i64 / 2;
    monitors.iter().min_by_key(|m| {
        let nx = cx.clamp(m.x as i64, m.right());
        let ny = cy.clamp(m.y as i64, m.bottom());
        (cx - nx).pow(2) + (cy - ny).pow(2)
    })
}

/// 窓が画面外に出ないよう左上座標を補正する。対象モニター内に横 MIN_VISIBLE px・縦はタイトルバーの
/// 位置（モニター上端以降）が残るようにする。モニター一覧が空なら補正せずそのまま返す。
pub fn clamp_slot_position(x: i32, y: i32, w: u32, h: u32, monitors: &[MonitorRect]) -> (i32, i32) {
    let Some(m) = target_monitor(x, y, w, h, monitors) else { return (x, y) };
    let min_x = m.x - w as i32 + MIN_VISIBLE;
    let max_x = (m.right() as i32 - MIN_VISIBLE).max(min_x);
    let min_y = m.y;
    let max_y = (m.bottom() as i32 - MIN_VISIBLE).max(min_y);
    (x.clamp(min_x, max_x), y.clamp(min_y, max_y))
}

/// 窓（外寸 outer_w×outer_h）をモニターの中央へ置く左上座標と、モニターに収まるよう縮めた外寸を返す。
/// 窓がモニターより大きいときだけ縮める。単位（論理/物理）は呼び出し側で揃えること。
pub fn center_in_monitor(outer_w: u32, outer_h: u32, m: &MonitorRect) -> (i32, i32, u32, u32) {
    let w = outer_w.min(m.w);
    let h = outer_h.min(m.h);
    let x = m.x + ((m.w - w) / 2) as i32;
    let y = m.y + ((m.h - h) / 2) as i32;
    (x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(x: i32, y: i32, w: u32, h: u32) -> MonitorRect { MonitorRect { x, y, w, h } }

    #[test]
    fn from_physical_divides_by_scale() {
        assert_eq!(MonitorRect::from_physical(3840, 0, 3840, 2160, 2.0), m(1920, 0, 1920, 1080));
        assert_eq!(MonitorRect::from_physical(10, 20, 100, 200, 0.0), m(10, 20, 100, 200), "不正な倍率は1扱い");
    }

    #[test]
    fn numbering_is_left_to_right_then_top_to_bottom() {
        let mut v = vec![m(1920, 0, 1920, 1080), m(-1920, 0, 1920, 1080), m(0, 0, 1920, 1080)];
        sort_for_numbering(&mut v);
        assert_eq!(v.iter().map(|m| m.x).collect::<Vec<_>>(), vec![-1920, 0, 1920]);
    }

    #[test]
    fn monitor_index_picks_largest_overlap_or_none() {
        let v = [m(0, 0, 1920, 1080), m(1920, 0, 1920, 1080)];
        assert_eq!(monitor_index_for_slot(100, 100, 800, 600, &v), Some(0));
        assert_eq!(monitor_index_for_slot(2000, 100, 800, 600, &v), Some(1));
        // 境界をまたぐ: 右モニターに多く重なる
        assert_eq!(monitor_index_for_slot(1700, 100, 800, 600, &v), Some(1));
        assert_eq!(monitor_index_for_slot(5000, 100, 800, 600, &v), None);
        assert_eq!(monitor_index_for_slot(0, 2000, 800, 600, &v), None);
    }

    #[test]
    fn clamp_keeps_valid_position_on_second_monitor() {
        // 旧実装は現モニターの幅だけで x を制限したため、2枚目(x=1920〜)の記録が1820に丸められた
        let v = [m(0, 0, 1920, 1080), m(1920, 0, 1920, 1080)];
        assert_eq!(clamp_slot_position(2000, 50, 800, 600, &v), (2000, 50));
    }

    #[test]
    fn clamp_supports_negative_origin_monitor() {
        let v = [m(-1920, 0, 1920, 1080), m(0, 0, 1920, 1080)];
        assert_eq!(clamp_slot_position(-1500, 100, 800, 600, &v), (-1500, 100));
    }

    #[test]
    fn clamp_pulls_offscreen_window_to_nearest_monitor() {
        let v = [m(0, 0, 1920, 1080)];
        // 右に飛び出し: 右端から MIN_VISIBLE 残る
        assert_eq!(clamp_slot_position(5000, 100, 800, 600, &v), (1820, 100));
        // 左に飛び出し: 左端から窓が MIN_VISIBLE だけ残る
        assert_eq!(clamp_slot_position(-5000, 100, 800, 600, &v), (-700, 100));
        // 上は 0 まで、下は下端から MIN_VISIBLE 残る
        assert_eq!(clamp_slot_position(100, -300, 800, 600, &v), (100, 0));
        assert_eq!(clamp_slot_position(100, 5000, 800, 600, &v), (100, 980));
    }

    #[test]
    fn clamp_with_removed_monitor_goes_to_remaining_one() {
        // 2枚目を抜いた後、2枚目にあった記録を展開 → 残った1枚目の中へ
        let v = [m(0, 0, 1920, 1080)];
        assert_eq!(clamp_slot_position(2500, 100, 800, 600, &v), (1820, 100));
    }

    #[test]
    fn clamp_without_monitors_is_noop() {
        assert_eq!(clamp_slot_position(-9999, 9999, 800, 600, &[]), (-9999, 9999));
    }

    #[test]
    fn center_in_monitor_centers_and_shrinks_only_when_too_big() {
        let mon = m(1920, 0, 1920, 1080);
        assert_eq!(center_in_monitor(800, 600, &mon), (1920 + 560, 240, 800, 600));
        assert_eq!(center_in_monitor(4000, 600, &mon), (1920, 240, 1920, 600));
        assert_eq!(center_in_monitor(1920, 1080, &mon), (1920, 0, 1920, 1080));
    }
}

//! 手動回転機能（TODO項目B）の状態・純粋ロジック。
//! Exif自動回転(A)・Exif ON/OFF設定(D、現時点では内部フラグのみ。設定UI/永続化はD本体の
//! 実装時に別途対応する)と組み合わせて最終表示角度を決める。
//!
//! ページ描画・見開き合成への配線はPhase2/3で行う。ここではUI/デコード層に依存しない
//! 純粋関数・状態のみを扱う。

use image::metadata::Orientation;

/// 角度を [0, 360) に正規化する。90度刻みの加減算を繰り返しても発散しないようにするため、
/// 回転操作のたびに必ずこれを通す。
pub fn normalize_360(deg: i32) -> i32 {
    ((deg % 360) + 360) % 360
}

/// Orientationから回転角度成分のみを取り出す（ミラー成分は無視）。
/// 通常のスキャン画像でミラー系タグが立つことはほぼ無いため、回転角度のみを
/// 手動回転の起点として扱う。
pub fn orientation_rotation_degrees(o: Orientation) -> i32 {
    match o {
        Orientation::NoTransforms   => 0,
        Orientation::Rotate90       => 90,
        Orientation::Rotate180      => 180,
        Orientation::Rotate270      => 270,
        Orientation::FlipHorizontal => 0,
        Orientation::FlipVertical   => 180,
        Orientation::Rotate90FlipH  => 90,
        Orientation::Rotate270FlipH => 270,
    }
}

/// 見開き/シングル表示でEXIF基準点を決めるためのページ参照。
/// 呼び出し側（cache/view_reader層）がどこからOrientationを取得するかには依存しない。
#[derive(Clone, Copy)]
pub struct PageOrientationRef {
    /// アーカイブ内ソート順インデックス
    pub index: usize,
    /// 仮想（詰め物）ページか
    pub is_virtual: bool,
    pub orientation: Orientation,
}

/// シングル/見開き共通のEXIF基準点決定。
/// 最優先分岐は「仮想ページでない方」、次点は「インデックスが小さい方」。
pub fn resolve_base_orientation(
    left: Option<&PageOrientationRef>,
    right: Option<&PageOrientationRef>,
) -> Orientation {
    match (left, right) {
        (Some(p), None) | (None, Some(p)) => p.orientation,
        (Some(l), Some(r)) => match (l.is_virtual, r.is_virtual) {
            (true, false) => r.orientation,
            (false, true) => l.orientation,
            (false, false) => if l.index <= r.index { l.orientation } else { r.orientation },
            (true, true) => Orientation::NoTransforms,
        },
        (None, None) => Orientation::NoTransforms,
    }
}

/// ページ単位の手動回転状態。
/// SpreadOffset同様、ファイルを開くたびに新規生成し、閉じると破棄する（使いまわし不可）。
/// rotation_carry_over が有効な間は、この値ではなく ViewerConfig::rotation_session_angle
/// が表示角度として使われる（呼び出し側の判断）。
pub struct RotationState {
    manual_delta: i32,
}

impl RotationState {
    pub fn new() -> Self {
        Self { manual_delta: 0 }
    }

    pub fn angle(&self) -> i32 {
        self.manual_delta
    }

    pub fn rotate_cw(&mut self) {
        self.manual_delta = normalize_360(self.manual_delta + 90);
    }

    pub fn rotate_ccw(&mut self) {
        self.manual_delta = normalize_360(self.manual_delta - 90);
    }

    /// 新規ビュー（ページ送り等で表示画像が差し替わった）時のリセット
    pub fn reset(&mut self) {
        self.manual_delta = 0;
    }

    /// D設定をOFF→ONに切り替えた瞬間の処理。
    /// 「EXIF値のみを見る」という意思表示になるため、手動加算分を破棄する。
    pub fn on_exif_enabled(&mut self) {
        self.manual_delta = 0;
    }
}

impl Default for RotationState {
    fn default() -> Self {
        Self::new()
    }
}

/// 最終表示角度を算出する。
///
/// - `base_exif_deg`: resolve_base_orientation の結果を orientation_rotation_degrees で
///   角度化したもの
/// - `exif_enabled`: D設定（現時点では内部フラグのみ）
/// - `manual_delta`: RotationState::angle()、もしくは carry_over 中は
///   ViewerConfig::rotation_session_angle
pub fn effective_angle(base_exif_deg: i32, exif_enabled: bool, manual_delta: i32) -> i32 {
    let start = if exif_enabled { base_exif_deg } else { 0 };
    normalize_360(start + manual_delta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_wraps_both_directions() {
        assert_eq!(normalize_360(0), 0);
        assert_eq!(normalize_360(360), 0);
        assert_eq!(normalize_360(450), 90);
        assert_eq!(normalize_360(-90), 270);
        assert_eq!(normalize_360(-360), 0);
    }

    #[test]
    fn rotation_state_wraps_after_four_cw() {
        let mut s = RotationState::new();
        for _ in 0..4 {
            s.rotate_cw();
        }
        assert_eq!(s.angle(), 0);
    }

    #[test]
    fn rotation_state_ccw_from_zero_wraps_to_270() {
        let mut s = RotationState::new();
        s.rotate_ccw();
        assert_eq!(s.angle(), 270);
    }

    #[test]
    fn orientation_rotation_degrees_drops_mirror_component() {
        assert_eq!(orientation_rotation_degrees(Orientation::FlipHorizontal), 0);
        assert_eq!(orientation_rotation_degrees(Orientation::FlipVertical), 180);
        assert_eq!(orientation_rotation_degrees(Orientation::Rotate90FlipH), 90);
        assert_eq!(orientation_rotation_degrees(Orientation::Rotate270FlipH), 270);
    }

    #[test]
    fn resolve_base_orientation_prefers_non_virtual() {
        let virtual_left = PageOrientationRef { index: 0, is_virtual: true, orientation: Orientation::Rotate90 };
        let real_right = PageOrientationRef { index: 1, is_virtual: false, orientation: Orientation::Rotate180 };
        assert_eq!(resolve_base_orientation(Some(&virtual_left), Some(&real_right)), Orientation::Rotate180);
    }

    #[test]
    fn resolve_base_orientation_prefers_lower_index_when_both_real() {
        let a = PageOrientationRef { index: 2, is_virtual: false, orientation: Orientation::Rotate90 };
        let b = PageOrientationRef { index: 3, is_virtual: false, orientation: Orientation::Rotate180 };
        assert_eq!(resolve_base_orientation(Some(&a), Some(&b)), Orientation::Rotate90);
    }

    #[test]
    fn effective_angle_ignores_exif_when_disabled() {
        assert_eq!(effective_angle(90, false, 90), 90);
        assert_eq!(effective_angle(90, true, 90), 180);
    }
}

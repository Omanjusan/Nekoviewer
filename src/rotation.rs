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

    /// D設定をON→OFFに切り替えた瞬間の処理。デコード時のEXIF焼き込みが無くなる分、
    /// 「見た目維持」のためEXIF回転角度ぶんを手動角度へ加算補正する。
    pub fn on_exif_disabled(&mut self, exif_deg: i32) {
        self.manual_delta = normalize_360(self.manual_delta + exif_deg);
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

/// 角度引き継ぎトグル(carry_over)の状態に応じて、ページ単位のRotationStateと
/// ViewerConfig::rotation_session_angle のどちらを更新するかを決める薄いラッパー。
/// ViewerState::rotate_cw/rotate_ccw から呼ばれる（UI/egui非依存で単体テスト可能にするため分離）。
pub fn rotate(carry_over: bool, page_state: &mut RotationState, session_angle: &mut i32, cw: bool) {
    if carry_over {
        *session_angle = normalize_360(*session_angle + if cw { 90 } else { -90 });
    } else if cw {
        page_state.rotate_cw();
    } else {
        page_state.rotate_ccw();
    }
}

/// carry_overの状態に応じて「今どちらの回転値が有効か」を返す。
pub fn manual_angle(carry_over: bool, page_state: &RotationState, session_angle: i32) -> i32 {
    if carry_over { session_angle } else { page_state.angle() }
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
    fn on_exif_enabled_discards_manual_delta() {
        let mut s = RotationState::new();
        s.rotate_cw();
        s.rotate_cw();
        s.on_exif_enabled();
        assert_eq!(s.angle(), 0);
    }

    #[test]
    fn on_exif_disabled_adds_and_normalizes_exif_degrees() {
        let mut s = RotationState::new();
        s.rotate_cw(); // manual_delta = 90
        s.on_exif_disabled(90); // 90 + 90 = 180
        assert_eq!(s.angle(), 180);
    }

    #[test]
    fn on_exif_disabled_wraps_across_360() {
        let mut s = RotationState::new();
        s.rotate_ccw(); // manual_delta = 270
        s.on_exif_disabled(180); // 270 + 180 = 450 -> 90
        assert_eq!(s.angle(), 90);
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

    #[test]
    fn rotate_updates_page_state_when_carry_over_off() {
        let mut page = RotationState::new();
        let mut session = 0;
        rotate(false, &mut page, &mut session, true);
        assert_eq!(page.angle(), 90);
        assert_eq!(session, 0, "carry_over無効時はsession_angleを触らない");
    }

    #[test]
    fn rotate_updates_session_angle_when_carry_over_on() {
        let mut page = RotationState::new();
        let mut session = 0;
        rotate(true, &mut page, &mut session, true);
        assert_eq!(session, 90);
        assert_eq!(page.angle(), 0, "carry_over有効時はpage_stateを触らない");
        rotate(true, &mut page, &mut session, false);
        assert_eq!(session, 0);
    }

    #[test]
    fn manual_angle_selects_source_by_carry_over() {
        let mut page = RotationState::new();
        page.rotate_cw();
        assert_eq!(manual_angle(false, &page, 270), 90, "OFF時はpage_state基準");
        assert_eq!(manual_angle(true, &page, 270), 270, "ON時はsession_angle基準");
    }
}

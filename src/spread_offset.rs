/// 見開きオフセット状態の種別
#[derive(Clone, Copy, PartialEq)]
pub enum SpreadDisplayState {
    /// 先頭仮想: spread_lo() == -1、[仮想ページ, 実ページ0]
    VirtualLeft,
    /// 整列: spread_lo() が偶数、[偶数p, 偶数p+1]
    Aligned,
    /// 1Pずれ: spread_lo() が奇数、[奇数p, 奇数p+1]
    ShiftedOne,
}

/// 見開きページオフセット状態。
/// ファイルを開くたびに新規生成し、ファイルを閉じると破棄する。使いまわし不可。
pub struct SpreadOffset {
    state: SpreadDisplayState,
    /// 末尾仮想フラグ: hi側（lo+1）に実ページが存在しない
    virtual_right: bool,
}

impl SpreadOffset {
    pub fn new() -> Self {
        Self { state: SpreadDisplayState::Aligned, virtual_right: false }
    }

    /// オフセット値 (-1 / 0 / +1)
    pub fn value(&self) -> i32 {
        match self.state {
            SpreadDisplayState::VirtualLeft => -1,
            SpreadDisplayState::Aligned    =>  0,
            SpreadDisplayState::ShiftedOne =>  1,
        }
    }

    /// 整列状態でないか（UI の「ずれ中」表示用）
    pub fn is_nonzero(&self) -> bool {
        !matches!(self.state, SpreadDisplayState::Aligned)
    }

    /// + 方向への調整が可能か
    /// ShiftedOne（+1 上限）または末尾仮想（これ以上進む実ページなし）のときは不可
    pub fn can_advance(&self) -> bool {
        !matches!(self.state, SpreadDisplayState::ShiftedOne) && !self.virtual_right
    }

    /// - 方向への調整が可能か
    /// VirtualLeft（-1 下限）のときは不可
    pub fn can_retreat(&self) -> bool {
        !matches!(self.state, SpreadDisplayState::VirtualLeft)
    }

    pub fn advance(&mut self) {
        if !self.can_advance() { return; }
        self.state = match self.state {
            SpreadDisplayState::VirtualLeft => SpreadDisplayState::Aligned,
            SpreadDisplayState::Aligned    => SpreadDisplayState::ShiftedOne,
            SpreadDisplayState::ShiftedOne => return,
        };
    }

    pub fn retreat(&mut self) {
        if !self.can_retreat() { return; }
        self.state = match self.state {
            SpreadDisplayState::VirtualLeft => return,
            SpreadDisplayState::Aligned    => SpreadDisplayState::VirtualLeft,
            SpreadDisplayState::ShiftedOne => SpreadDisplayState::Aligned,
        };
    }

    pub fn reset(&mut self) {
        self.state = SpreadDisplayState::Aligned;
        self.virtual_right = false;
    }

    /// viewer が毎フレーム末尾仮想フラグを更新する
    /// hi側ページ（lo+1）が存在しないときに true を渡す
    pub fn update_virtual_right(&mut self, at_end: bool) {
        self.virtual_right = at_end;
    }
}

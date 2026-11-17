//! UI のベースカラー（ダーク／ライト）。システム連動は持たない（winit 0.30 が Linux で OS テーマを
//! 渡さないため。ThemePreference::System を使わず、常に Dark / Light を明示して egui に渡す）。
//! `nekoviewer.state` の `theme=dark|light` に永続化する。

/// UI のベースカラー。既定はダーク（従来の見た目）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum ThemeMode {
    #[default]
    Dark,
    Light,
}

impl ThemeMode {
    /// ダーク⇄ライトを入れ替える。
    pub(crate) fn toggled(self) -> Self {
        match self {
            ThemeMode::Dark => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
        }
    }

    /// state ファイルに書く値（`theme=` の右辺）。
    pub(crate) fn as_state_str(self) -> &'static str {
        match self {
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
        }
    }

    /// state ファイルの値から復元する。未記載・不明な値は既定（ダーク）。
    pub(crate) fn from_state_str(s: &str) -> Self {
        match s.trim() {
            "light" => ThemeMode::Light,
            _ => ThemeMode::Dark,
        }
    }

    pub(crate) fn to_egui(self) -> egui::Theme {
        match self {
            ThemeMode::Dark => egui::Theme::Dark,
            ThemeMode::Light => egui::Theme::Light,
        }
    }

    /// 指定した窓の `Context` に適用する。
    pub(crate) fn apply(self, ctx: &egui::Context) {
        ctx.set_theme(self.to_egui());
    }
}

/// 固定の暗色背景（半透明の黒など）の上に置く子 Ui を、テーマに関わらずダークの見た目にする。
/// ライト時に「暗い背景に暗い文字」になるのを防ぐ。ビューアーの画像上オーバーレイ
/// （ツールパレット、サムネバー）のように、背景色が画像前提で固定のものが対象。
/// スタイルの他の項目（余白など）は触らず、`visuals` だけを差し替える。
pub(crate) fn force_dark_visuals(ui: &mut egui::Ui) {
    let dark = ui.ctx().style_of(egui::Theme::Dark).visuals.clone();
    ui.style_mut().visuals = dark;
}

#[cfg(test)]
mod tests {
    use super::ThemeMode;

    #[test]
    fn default_is_dark() {
        assert_eq!(ThemeMode::default(), ThemeMode::Dark);
    }

    #[test]
    fn toggled_flips_and_returns() {
        assert_eq!(ThemeMode::Dark.toggled(), ThemeMode::Light);
        assert_eq!(ThemeMode::Light.toggled(), ThemeMode::Dark);
        assert_eq!(ThemeMode::Dark.toggled().toggled(), ThemeMode::Dark);
    }

    #[test]
    fn state_str_round_trips_and_unknown_falls_back_to_dark() {
        for m in [ThemeMode::Dark, ThemeMode::Light] {
            assert_eq!(ThemeMode::from_state_str(m.as_state_str()), m);
        }
        assert_eq!(ThemeMode::from_state_str(""), ThemeMode::Dark);
        assert_eq!(ThemeMode::from_state_str("system"), ThemeMode::Dark);
        assert_eq!(ThemeMode::from_state_str(" light \n"), ThemeMode::Light);
    }

    #[test]
    fn maps_to_egui_theme() {
        assert_eq!(ThemeMode::Dark.to_egui(), egui::Theme::Dark);
        assert_eq!(ThemeMode::Light.to_egui(), egui::Theme::Light);
    }
}

//! UI のベースカラー（ダーク／ライト）。システム連動は持たない（winit 0.30 が Linux で OS テーマを
//! 渡さないため。ThemePreference::System を使わず、常に Dark / Light を明示して egui に渡す）。

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
    fn maps_to_egui_theme() {
        assert_eq!(ThemeMode::Dark.to_egui(), egui::Theme::Dark);
        assert_eq!(ThemeMode::Light.to_egui(), egui::Theme::Light);
    }
}

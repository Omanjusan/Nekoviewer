//! 名前の「完全一致」と「類似」の判定（画面・DB・ファイルI/Oに触れない純関数）。
//!
//! 一括タグ付けで、既存のタグ要素との照合と、候補どうしの束ね（類似品のまとめ）に使う。
//!
//! - **完全一致キー**（`exact_key`）: 正規化4項目（全角半角・スペース・ハイフンとアンダーバー・特殊文字）だけを適用する。
//!   大文字小文字は常に同一視する。キーが等しければ「完全一致」
//! - **類似キー**（`fuzzy_key`）: 完全一致キーに、ローマ字の表記ゆれの吸収（5つ目の項目）を足したもの。
//!   仮名はローマ字に直して比べる（`コウジ` と `Kouji` が近くなる）。漢字は読みを推測できないので、そのまま
//! - **類似度**（`similarity`）: 0〜100。完全一致は100。類似キー同士の編集距離から求め、完全一致でない限り99を超えない

// 候補生成（C2の続き）と確認ウィンドウ（D）が使う。それまでは未使用の警告を出さない。
#![allow(dead_code)]

/// 照合の切替項目。確認ウィンドウで切り替え、その場で再計算する。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MatchOptions {
    /// 全角英数記号→半角、全角空白→空白、半角カナ→全角カナ。
    pub width: bool,
    /// 空白を無視する。
    pub space: bool,
    /// ハイフン・アンダーバー（全角・長音記号の類を含む）を無視する。
    pub hyphen_underscore: bool,
    /// その他の特殊文字（括弧・句読点・記号）を取り除く。
    pub symbols: bool,
    /// ローマ字の表記ゆれ（kouji/kohji/koji、si/shi、tu/tsu など）と、仮名のローマ字化を類似判定に使う。
    pub romaji: bool,
    /// 既存タグへの「類似マッチ」を提示する類似度のしきい値（%）。
    pub threshold: u8,
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self { width: true, space: true, hyphen_underscore: true, symbols: true, romaji: true, threshold: 80 }
    }
}

impl MatchOptions {
    /// state ファイル用の文字列（全角半角,スペース,ハイフン,特殊文字,ローマ字,しきい値）。例: `1,1,1,1,1,80`。
    pub(crate) fn to_state_str(self) -> String {
        let b = |v: bool| u8::from(v);
        format!("{},{},{},{},{},{}", b(self.width), b(self.space), b(self.hyphen_underscore), b(self.symbols), b(self.romaji), self.threshold)
    }

    /// `to_state_str` の逆。個数や値が不正なら None（呼び出し側は既定値のまま）。しきい値は丸める。
    pub(crate) fn from_state_str(s: &str) -> Option<Self> {
        let p: Vec<&str> = s.trim().split(',').map(str::trim).collect();
        let [w, sp, h, sy, r, th] = p.as_slice() else { return None };
        let flag = |v: &str| match v {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        };
        Some(
            Self {
                width: flag(w)?,
                space: flag(sp)?,
                hyphen_underscore: flag(h)?,
                symbols: flag(sy)?,
                romaji: flag(r)?,
                threshold: th.parse().ok()?,
            }
            .clamped(),
        )
    }

    /// しきい値を 50〜100 に丸める。
    pub(crate) fn clamped(mut self) -> Self {
        self.threshold = self.threshold.clamp(50, 100);
        self
    }
}

// ── 全角半角 ──────────────────────────────────────────────────────────

/// 半角カナ（U+FF66〜FF9F）を全角カナにする。濁点・半濁点（ﾞﾟ）は直前の文字に合成する。
fn halfwidth_kana_to_full(s: &str) -> String {
    const BASE: &str = "ヲァィゥェォャュョッーアイウエオカキクケコサシスセソタチツテトナニヌネノハヒフヘホマミムメモヤユヨラリルレロワン";
    // U+FF66 から FF9D まで（ヲ ァ ィ ゥ ェ ォ ャ ュ ョ ッ ー ア…ン）。
    let full: Vec<char> = BASE.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(s.len());
    for c in s.chars() {
        let u = c as u32;
        match u {
            0xFF66..=0xFF9D => out.push(full[(u - 0xFF66) as usize]),
            0xFF9E | 0xFF9F => {
                let voiced = u == 0xFF9E;
                if let Some(prev) = out.last().copied() {
                    if let Some(merged) = merge_voicing(prev, voiced) {
                        *out.last_mut().unwrap() = merged;
                        continue;
                    }
                }
                out.push(if voiced { '゛' } else { '゜' });
            }
            _ => out.push(c),
        }
    }
    out.into_iter().collect()
}

/// 全角カナに濁点・半濁点を付けた文字（付けられなければ None）。
fn merge_voicing(c: char, voiced: bool) -> Option<char> {
    const PLAIN: &str = "カキクケコサシスセソタチツテトハヒフヘホウ";
    const DAKU: &str = "ガギグゲゴザジズゼゾダヂヅデドバビブベボヴ";
    const HANDAKU: &str = "パピプペポ";
    if voiced {
        let i = PLAIN.chars().position(|p| p == c)?;
        DAKU.chars().nth(i)
    } else {
        let i = "ハヒフヘホ".chars().position(|p| p == c)?;
        HANDAKU.chars().nth(i)
    }
}

fn apply_width(s: &str) -> String {
    let converted: String = s
        .chars()
        .map(|c| match c as u32 {
            0xFF01..=0xFF5E => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            0x3000 => ' ',
            _ => c,
        })
        .collect();
    halfwidth_kana_to_full(&converted)
}

/// 登録名用の全角半角の正規化（全角英数記号→半角、全角空白→空白、半角カナ→全角カナ）。大文字小文字は変えない。
pub(crate) fn normalize_width(s: &str) -> String {
    apply_width(s)
}

fn is_hyphen_like(c: char) -> bool {
    matches!(c, '-' | '_' | '‐' | '‑' | '–' | '—' | '−' | '＿' | '－' | 'ー' | '～' | '〜' | '~')
}

fn is_symbol(c: char) -> bool {
    // 英数字・仮名・漢字・ハングルなど「文字」以外のうち、空白とハイフン類を除くもの。
    !c.is_alphanumeric() && !c.is_whitespace() && !is_hyphen_like(c) && !is_combining_mark(c)
}

fn is_combining_mark(c: char) -> bool {
    crate::fs::archive::meta::is_combining(c)
}

// ── 仮名 → ローマ字 ───────────────────────────────────────────────────

/// ひらがな・カタカナ1文字のローマ字（拗音の小書き文字・促音・長音は別処理）。
fn kana_base(c: char) -> Option<&'static str> {
    Some(match c {
        'あ' => "a", 'い' => "i", 'う' => "u", 'え' => "e", 'お' => "o",
        'か' => "ka", 'き' => "ki", 'く' => "ku", 'け' => "ke", 'こ' => "ko",
        'が' => "ga", 'ぎ' => "gi", 'ぐ' => "gu", 'げ' => "ge", 'ご' => "go",
        'さ' => "sa", 'し' => "shi", 'す' => "su", 'せ' => "se", 'そ' => "so",
        'ざ' => "za", 'じ' => "ji", 'ず' => "zu", 'ぜ' => "ze", 'ぞ' => "zo",
        'た' => "ta", 'ち' => "chi", 'つ' => "tsu", 'て' => "te", 'と' => "to",
        'だ' => "da", 'ぢ' => "ji", 'づ' => "zu", 'で' => "de", 'ど' => "do",
        'な' => "na", 'に' => "ni", 'ぬ' => "nu", 'ね' => "ne", 'の' => "no",
        'は' => "ha", 'ひ' => "hi", 'ふ' => "fu", 'へ' => "he", 'ほ' => "ho",
        'ば' => "ba", 'び' => "bi", 'ぶ' => "bu", 'べ' => "be", 'ぼ' => "bo",
        'ぱ' => "pa", 'ぴ' => "pi", 'ぷ' => "pu", 'ぺ' => "pe", 'ぽ' => "po",
        'ま' => "ma", 'み' => "mi", 'む' => "mu", 'め' => "me", 'も' => "mo",
        'や' => "ya", 'ゆ' => "yu", 'よ' => "yo",
        'ら' => "ra", 'り' => "ri", 'る' => "ru", 'れ' => "re", 'ろ' => "ro",
        'わ' => "wa", 'を' => "o", 'ん' => "n", 'ゐ' => "i", 'ゑ' => "e", 'ゔ' => "vu",
        'ぁ' => "a", 'ぃ' => "i", 'ぅ' => "u", 'ぇ' => "e", 'ぉ' => "o",
        _ => return None,
    })
}

/// 小書きの拗音（ゃゅょ）の母音。
fn small_y(c: char) -> Option<char> {
    match c {
        'ゃ' => Some('a'),
        'ゅ' => Some('u'),
        'ょ' => Some('o'),
        _ => None,
    }
}

/// カタカナをひらがなにする（長音記号・小書き・濁点類はそのまま通す）。
fn to_hiragana(c: char) -> char {
    match c as u32 {
        0x30A1..=0x30F6 => char::from_u32(c as u32 - 0x60).unwrap_or(c),
        _ => c,
    }
}

/// ひらがな・カタカナをヘボン式寄りのローマ字にする。漢字・英数字・記号はそのまま通す。
/// 促音は次の子音を重ね、長音記号は直前の母音を繰り返す。
pub(crate) fn kana_to_romaji(s: &str) -> String {
    let chars: Vec<char> = s.chars().map(to_hiragana).collect();
    let mut out = String::new();
    let mut i = 0;
    let mut double_next = false;
    while i < chars.len() {
        let c = chars[i];
        if c == 'っ' {
            double_next = true;
            i += 1;
            continue;
        }
        if c == 'ー' {
            if let Some(v) = out.chars().rev().find(|ch| matches!(ch, 'a' | 'i' | 'u' | 'e' | 'o')) {
                out.push(v);
            }
            i += 1;
            continue;
        }
        let Some(base) = kana_base(c) else {
            double_next = false;
            out.push(c);
            i += 1;
            continue;
        };
        // 拗音: 「き」+「ゃ」→ kya。し・ち・じ・ぢは sha / cha / ja。
        let mut piece = base.to_string();
        if let Some(v) = chars.get(i + 1).copied().and_then(small_y) {
            if matches!(c, 'き' | 'し' | 'ち' | 'に' | 'ひ' | 'み' | 'り' | 'ぎ' | 'じ' | 'ぢ' | 'び' | 'ぴ') {
                let stem = match c {
                    'し' => "sh",
                    'ち' => "ch",
                    'じ' | 'ぢ' => "j",
                    other => &kana_base(other).unwrap_or("")[..kana_base(other).map_or(0, |b| b.len() - 1)],
                };
                piece = format!("{stem}{}", if matches!(c, 'き' | 'に' | 'ひ' | 'み' | 'り' | 'ぎ' | 'び' | 'ぴ') { format!("y{v}") } else { v.to_string() });
                i += 1;
            }
        }
        if double_next {
            // ch は t を重ねる（matcha → mattcha ではなく mat-cha の慣用）。それ以外は先頭の子音。
            if let Some(first) = piece.chars().next().filter(|ch| !matches!(ch, 'a' | 'i' | 'u' | 'e' | 'o' | 'n')) {
                out.push(if piece.starts_with("ch") { 't' } else { first });
            }
            double_next = false;
        }
        out.push_str(&piece);
        i += 1;
    }
    out
}

/// ローマ字の表記ゆれを吸収する（入力は小文字のローマ字）。
/// 長音（ou / oh / oo / uu → o / u）、si→shi、ti→chi、tu→tsu、hu→fu、zi→ji、sya→sha、tya→cha、zya→ja など。
pub(crate) fn fold_romaji(s: &str) -> String {
    let src: Vec<char> = s
        .chars()
        .map(|c| match c {
            'ā' => 'a',
            'ī' => 'i',
            'ū' => 'u',
            'ē' => 'e',
            'ō' => 'o',
            other => other,
        })
        .collect();
    let at = |i: usize| src.get(i).copied();
    let is_vowel = |c: Option<char>| matches!(c, Some('a' | 'i' | 'u' | 'e' | 'o'));
    let mut out = String::with_capacity(src.len() + 4);
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        let (n1, n2) = (at(i + 1), at(i + 2));
        match (c, n1, n2) {
            // 拗音の訓令式・日本式 → ヘボン式
            ('s', Some('y'), Some(v @ ('a' | 'u' | 'o'))) => {
                out.push_str("sh");
                out.push(v);
                i += 3;
            }
            ('t', Some('y'), Some(v @ ('a' | 'u' | 'o'))) => {
                out.push_str("ch");
                out.push(v);
                i += 3;
            }
            ('z' | 'j', Some('y'), Some(v @ ('a' | 'u' | 'o'))) => {
                out.push('j');
                out.push(v);
                i += 3;
            }
            ('s', Some('i'), _) => {
                out.push_str("shi");
                i += 2;
            }
            ('t', Some('i'), _) => {
                out.push_str("chi");
                i += 2;
            }
            ('t', Some('u'), _) => {
                out.push_str("tsu");
                i += 2;
            }
            ('z' | 'd', Some('i'), _) => {
                out.push_str("ji");
                i += 2;
            }
            // hu → fu（shu・chu の u を誤って変えないよう、直前が s・c のときは対象外）
            ('h', Some('u'), _) if !matches!(i.checked_sub(1).and_then(|p| src.get(p)).copied(), Some('s' | 'c')) => {
                out.push_str("fu");
                i += 2;
            }
            // 長音
            ('o', Some('u' | 'o'), _) => {
                out.push('o');
                i += 2;
            }
            ('o', Some('h'), next) if !is_vowel(next) => {
                out.push('o');
                i += 2;
            }
            ('u', Some('u'), _) => {
                out.push('u');
                i += 2;
            }
            ('n', Some('\''), _) => {
                out.push('n');
                i += 2;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

// ── キー・類似度 ──────────────────────────────────────────────────────

/// 完全一致キー: 正規化4項目だけを適用し、小文字にそろえる。
pub(crate) fn exact_key(name: &str, opts: &MatchOptions) -> String {
    let s = if opts.width { apply_width(name) } else { name.to_string() };
    s.chars()
        .flat_map(char::to_lowercase)
        .filter(|&c| {
            !(opts.space && c.is_whitespace()) && !(opts.hyphen_underscore && is_hyphen_like(c)) && !(opts.symbols && is_symbol(c))
        })
        .collect()
}

/// 類似キー: 完全一致キーに、ローマ字の表記ゆれの吸収を足す。
pub(crate) fn fuzzy_key(name: &str, opts: &MatchOptions) -> String {
    if !opts.romaji {
        return exact_key(name, opts);
    }
    // 仮名のローマ字化は、長音記号が消える前（ハイフン無視より先）に行う。
    let pre = if opts.width { apply_width(name) } else { name.to_string() };
    let romaji = kana_to_romaji(&pre.to_lowercase());
    let key = exact_key(&romaji, opts);
    fold_romaji(&key)
}

fn edit_distance(a: &[char], b: &[char]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// 編集距離が `bound` 以下ならその距離、超えると分かった時点で None。
fn bounded_edit_distance(a: &[char], b: &[char], bound: usize) -> Option<usize> {
    if a.len().abs_diff(b.len()) > bound {
        return None;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        let mut row_min = cur[0];
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
            row_min = row_min.min(cur[j + 1]);
        }
        if row_min > bound {
            return None;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    (prev[b.len()] <= bound).then_some(prev[b.len()])
}

/// 類似キー同士の類似度（0〜100）。どちらかが空なら 0。
fn key_similarity(a: &str, b: &str) -> u8 {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    if a == b {
        return 100;
    }
    let (ca, cb): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let max = ca.len().max(cb.len());
    // 長さの差だけで下限が決まる（編集距離は長さの差以上）。大きく違うものは距離を計算しない。
    let min_d = ca.len().abs_diff(cb.len());
    if min_d * 100 / max > 60 {
        return (100usize.saturating_sub(min_d * 100 / max)) as u8;
    }
    let d = edit_distance(&ca, &cb);
    (100usize.saturating_sub(d * 100 / max)) as u8
}

/// 2つの名前の類似度（0〜100）。完全一致キーが等しければ100、それ以外は99を超えない。
pub(crate) fn similarity(a: &str, b: &str, opts: &MatchOptions) -> u8 {
    similarity_keys(&keys(a, opts), &keys(b, opts))
}

/// 1つの名前の照合キー（事前計算して、多数の相手との比較で使い回す）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Keys {
    pub exact: String,
    pub fuzzy: String,
    /// `fuzzy` を文字の配列にしたもの（編集距離の計算で毎回作り直さないため）。
    chars: Vec<char>,
}

pub(crate) fn keys(name: &str, opts: &MatchOptions) -> Keys {
    let fuzzy = fuzzy_key(name, opts);
    let chars = fuzzy.chars().collect();
    Keys { exact: exact_key(name, opts), fuzzy, chars }
}

/// 事前計算したキーどうしの類似度（`similarity` と同じ規則）。
pub(crate) fn similarity_keys(a: &Keys, b: &Keys) -> u8 {
    if !a.exact.is_empty() && a.exact == b.exact {
        return 100;
    }
    if a.chars.is_empty() || b.chars.is_empty() {
        return 0;
    }
    if a.chars == b.chars {
        return 99;
    }
    let max = a.chars.len().max(b.chars.len());
    let d = edit_distance(&a.chars, &b.chars);
    (100usize.saturating_sub(d * 100 / max)).min(99) as u8
}

/// 類似度が `threshold` 以上のときだけ値を返す。届かないと分かった時点で距離の計算を打ち切るので、
/// 多数の相手との比較（既存タグの検索・類似品の束ね）はこちらを使う。
pub(crate) fn similarity_at_least(a: &Keys, b: &Keys, threshold: u8) -> Option<u8> {
    if !a.exact.is_empty() && a.exact == b.exact {
        return Some(100);
    }
    if a.chars.is_empty() || b.chars.is_empty() {
        return None;
    }
    if a.chars == b.chars {
        // 完全一致ではない（類似キーだけが同じ）ので99。しきい値が100なら届かない。
        return (threshold <= 99).then_some(99);
    }
    let max = a.chars.len().max(b.chars.len());
    // 類似度 = 100 - d*100/max >= threshold となる最大の d。
    let bound = ((101 - threshold as usize) * max).saturating_sub(1) / 100;
    let d = bounded_edit_distance(&a.chars, &b.chars, bound)?;
    Some((100usize.saturating_sub(d * 100 / max)).min(99) as u8)
}

/// 完全一致か（正規化4項目だけで同じか）。
pub(crate) fn is_exact(a: &str, b: &str, opts: &MatchOptions) -> bool {
    let (ea, eb) = (exact_key(a, opts), exact_key(b, opts));
    !ea.is_empty() && ea == eb
}

/// `names` を、類似度が `opts.threshold` 以上のものどうしで束ねる（連鎖でつながるものも同じ束）。
/// 戻り値は各名前の束の番号（先に出た名前の番号から振る）。完全一致キーが同じ名前は必ず同じ束。
pub(crate) fn cluster(names: &[String], opts: &MatchOptions) -> Vec<usize> {
    let ks: Vec<Keys> = names.iter().map(|n| keys(n, opts)).collect();
    let mut parent: Vec<usize> = (0..names.len()).collect();
    fn find(p: &mut Vec<usize>, x: usize) -> usize {
        let mut r = x;
        while p[r] != r {
            r = p[r];
        }
        let mut c = x;
        while p[c] != r {
            let n = p[c];
            p[c] = r;
            c = n;
        }
        r
    }
    for i in 0..names.len() {
        for j in (i + 1)..names.len() {
            if find(&mut parent, i) == find(&mut parent, j) {
                continue;
            }
            if similarity_at_least(&ks[i], &ks[j], opts.threshold).is_some() {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                parent[rj] = ri;
            }
        }
    }
    let mut ids: Vec<usize> = Vec::with_capacity(names.len());
    let mut roots: Vec<usize> = Vec::new();
    for i in 0..names.len() {
        let r = find(&mut parent, i);
        let id = match roots.iter().position(|&x| x == r) {
            Some(p) => p,
            None => {
                roots.push(r);
                roots.len() - 1
            }
        };
        ids.push(id);
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o() -> MatchOptions {
        MatchOptions::default()
    }

    #[test]
    fn exact_key_applies_the_four_toggles_independently() {
        let all = o();
        assert_eq!(exact_key("ＡＢＣ　Ｄ_e-f.g", &all), "abcdefg");
        let no_width = MatchOptions { width: false, ..all };
        assert_ne!(exact_key("ＡＢＣ", &no_width), exact_key("abc", &no_width), "全角半角の吸収をOFF");
        assert_eq!(exact_key("ABC", &no_width), "abc", "大文字小文字は常に同一視");
        let no_space = MatchOptions { space: false, ..all };
        assert_eq!(exact_key("a b", &no_space), "a b");
        let no_hy = MatchOptions { hyphen_underscore: false, ..all };
        assert_eq!(exact_key("a-b_c", &no_hy), "a-b_c");
        assert_eq!(exact_key("a-b_c", &all), "abc");
        let no_sym = MatchOptions { symbols: false, ..all };
        assert_eq!(exact_key("a(b)!", &no_sym), "a(b)!");
        assert_eq!(exact_key("a(b)!", &all), "ab");
        // 日本語の文字は記号として落とさない。
        assert_eq!(exact_key("作者・こうじ", &all), "作者こうじ".replace('・', ""));
    }

    #[test]
    fn options_state_string_round_trips_and_rejects_garbage() {
        let o = MatchOptions { width: false, space: true, hyphen_underscore: false, symbols: true, romaji: false, threshold: 90 };
        assert_eq!(o.to_state_str(), "0,1,0,1,0,90");
        assert_eq!(MatchOptions::from_state_str(&o.to_state_str()), Some(o));
        assert_eq!(MatchOptions::default().to_state_str(), "1,1,1,1,1,80");
        for bad in ["", "1,1,1", "1,1,1,1,1", "1,1,1,1,1,x", "2,1,1,1,1,80", "1,1,1,1,1,80,1"] {
            assert_eq!(MatchOptions::from_state_str(bad), None, "{bad}");
        }
        assert_eq!(MatchOptions::from_state_str("1,1,1,1,1,10").map(|o| o.threshold), Some(50), "しきい値は丸める");
        assert_eq!(MatchOptions::from_state_str("1,1,1,1,1,255").map(|o| o.threshold), Some(100));
    }

    #[test]
    fn normalize_width_keeps_case_and_widens_halfwidth_kana() {
        assert_eq!(normalize_width("ＡＢＣ　ｄｅｆ"), "ABC def");
        assert_eq!(normalize_width("ｺｳｼﾞ"), "コウジ");
        assert_eq!(normalize_width("Hawk Bit"), "Hawk Bit");
    }

    #[test]
    fn halfwidth_katakana_is_widened_with_voicing() {
        let all = o();
        assert_eq!(exact_key("ｺｳｼﾞ", &all), "コウジ");
        assert_eq!(exact_key("ﾊﾟﾝ", &all), "パン");
        assert_eq!(exact_key("ｶﾞｷﾞ", &all), "ガギ");
    }

    #[test]
    fn kana_to_romaji_covers_small_kana_sokuon_and_long_vowels() {
        assert_eq!(kana_to_romaji("こうじ"), "kouji");
        assert_eq!(kana_to_romaji("コウジ"), "kouji");
        assert_eq!(kana_to_romaji("しゃしん"), "shashin");
        assert_eq!(kana_to_romaji("ちょこ"), "choko");
        assert_eq!(kana_to_romaji("きょう"), "kyou");
        assert_eq!(kana_to_romaji("まっちゃ"), "matcha");
        assert_eq!(kana_to_romaji("ざっし"), "zasshi");
        assert_eq!(kana_to_romaji("ホーク"), "hooku");
        assert_eq!(kana_to_romaji("漢字abc"), "漢字abc", "漢字・英字はそのまま");
    }

    #[test]
    fn romaji_variants_fold_to_the_same_key() {
        let k = |s: &str| fold_romaji(s);
        assert_eq!(k("kouji"), k("kohji"));
        assert_eq!(k("kouji"), k("koji"));
        assert_eq!(k("kōji"), k("koji"));
        assert_eq!(k("sinya"), k("shinya"));
        assert_eq!(k("tuyoshi"), k("tsuyoshi"));
        assert_eq!(k("huji"), k("fuji"));
        assert_eq!(k("zinzya"), k("jinja"));
        assert_eq!(k("tyoko"), k("choko"));
        assert_eq!(k("syun"), k("shun"));
        assert_eq!(k("shun"), "shun", "shu の u は fu にしない");
        assert_eq!(k("chuu"), k("chu"));
        // 長音の h は、次が母音のときは残す（ohara の h は子音）。
        assert_eq!(k("ohara"), "ohara");
    }

    #[test]
    fn cluster_groups_exact_and_similar_names_and_keeps_unrelated_apart() {
        let all = o();
        let names: Vec<String> = ["Kouji", "kohji", "KOJI", "Hawk Bit", "hawk-bit", "HawkBit", "Takahama", "こうじ"]
            .map(String::from)
            .to_vec();
        let c = cluster(&names, &all);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[0], c[2]);
        assert_eq!(c[0], c[7], "仮名もローマ字と同じ束");
        assert_eq!(c[3], c[4]);
        assert_eq!(c[3], c[5]);
        assert_ne!(c[0], c[3]);
        assert_ne!(c[3], c[6]);
        assert_eq!(c[0], 0, "束の番号は先に出た名前から振る");
        // しきい値100なら、完全一致だけが束になる。
        let strict = MatchOptions { threshold: 100, ..all };
        let c = cluster(&names, &strict);
        assert_ne!(c[0], c[1], "Kouji と kohji は完全一致ではない");
        assert_eq!(c[3], c[4]);
        // ローマ字をOFFにすると、綴りの違いは束にならない。
        let no_romaji = MatchOptions { romaji: false, threshold: 90, ..all };
        let c = cluster(&names, &no_romaji);
        assert_ne!(c[0], c[2], "Kouji と KOJI は綴りが違う");
        assert!(cluster(&[], &all).is_empty());
    }

    #[test]
    fn similarity_is_100_only_for_exact_and_romaji_variants_rank_just_below() {
        let all = o();
        assert_eq!(similarity("Hawk Bit", "hawk-bit", &all), 100);
        assert_eq!(similarity("ＫＯＵＪＩ", "kouji", &all), 100);
        let s = similarity("Kouji", "Koji", &all);
        assert!((90..=99).contains(&s), "ローマ字ゆれは99以下: {s}");
        assert_eq!(similarity("コウジ", "Kouji", &all), 99, "仮名とローマ字は同じ類似キー（完全一致ではない）");
        assert_eq!(similarity("こうじ", "コウジ", &all), 99, "ひらがなとカタカナは完全一致ではなく類似（同じ読み）");
    }
}

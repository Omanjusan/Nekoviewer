//! 翻訳機能(実験的)。現段階ではOCR抽出のみ。ローカルAI(Ollama/OpenWebUI等の
//! OpenAI互換API)へHTTPで接続し、疎通確認・vision能力確認・OCRリクエストを行う。
//! クラウドAPIは対象外（APIキー管理は今回のスコープ外）。

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 翻訳機能で扱う言語（原文言語・翻訳先言語の両方で共有する）。
/// i18n::Lang（アプリUI表示言語）とは別概念のため名前を分けている。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TranslateLang {
    Japanese,
    ChineseSimplified,
    ChineseTraditional,
    English,
    Korean,
}

impl TranslateLang {
    pub const ALL: [TranslateLang; 5] = [
        TranslateLang::Japanese,
        TranslateLang::ChineseSimplified,
        TranslateLang::ChineseTraditional,
        TranslateLang::English,
        TranslateLang::Korean,
    ];
}

/// 「翻訳機能」設定タブで編集する永続設定。
#[derive(Clone)]
pub struct TranslateConfig {
    /// OpenAI互換APIのベースURL（例: http://172.17.0.1:11434）。空文字 = 未設定。
    pub base_url: String,
    /// 翻訳に使うモデル名。設定UI上は上位（主）の項目で、OCRモデルは既定でこれに追従する。
    pub translation_model: String,
    /// OCRに使うモデル名。設定UIでユーザーが明示的に選び直すまでは翻訳モデルに追従する。
    pub ocr_model: String,
    /// オーバーレイウィンドウの横幅(px)。
    /// 現状どの描画コードからも参照されていない未使用設定（旧・画面隅フローティング
    /// オーバーレイ用に用意されたが、子ウィンドウは480x640固定サイズで運用されている）。
    /// 設定タブ・state永続化には残っているため、値自体は保持しておく。
    pub overlay_width: u32,
}

impl Default for TranslateConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            translation_model: String::new(),
            ocr_model: String::new(),
            overlay_width: 360,
        }
    }
}

/// オーバーレイ横幅スライダーの下限・上限(px)。
pub const OVERLAY_WIDTH_FLOOR: u32 = 160;
pub const OVERLAY_WIDTH_CEILING: u32 = 800;

/// vision能力チェックに使うプローブ画像(64x64 赤、PNG)。色を尋ねて応答に反映されるか見るだけの
/// 軽量な疎通確認用で、実OCR用途の画質とは無関係。
/// 注意: Qwen3-VL系のSmartResize処理はリサイズ係数(factor=32)未満の画像でpanicするため、
/// 32px未満の極小画像は使わないこと（実際に8x8で `model runner has unexpectedly stopped` を誘発した実績あり）。
const PROBE_IMAGE_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAYklEQVR4nO3PMQ0AIADAMEAI/kUhBhEcDcmqYJtn7/GzpQNeNaA1oDWgNaA1oDWgNaA1oDWgNaA1oDWgNaA1oDWgNaA1oDWgNaA1oDWgNaA1oDWgNaA1oDWgNaA1oDWgNaBdCJ0BmMJ25zMAAAAASUVORK5CYII=";

#[derive(Deserialize)]
struct ModelsListResp {
    data: Vec<ModelsListEntry>,
}

#[derive(Deserialize)]
struct ModelsListEntry {
    id: String,
}

#[derive(Serialize)]
struct ChatMessageContentText<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
}

#[derive(Serialize)]
struct ChatMessageContentImage {
    #[serde(rename = "type")]
    kind: &'static str,
    image_url: ImageUrl,
}

#[derive(Serialize)]
struct ImageUrl {
    url: String,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ChatContentPart<'a> {
    Text(ChatMessageContentText<'a>),
    Image(ChatMessageContentImage),
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'static str,
    content: Vec<ChatContentPart<'a>>,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    /// 未指定(None)ならサーバ既定値。reasoningモデルは画像解析の思考に予算を消費しがちで、
    /// 既定値のままだと最終回答(content)を出す前に打ち切られ空応答になることがあるため、
    /// OCRリクエストでは大きめの値を明示する。
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: String,
    /// reasoningモデル(qwen系等)が思考過程を分離して返すフィールド。contentが空だった際の
    /// フォールバック抽出元として使う（打ち切られてcontentへ回答をコピーし損ねた場合の救済）。
    #[serde(default)]
    reasoning: Option<String>,
}

/// 接続チェックの進行状態。設定ダイアログ側でこれを見てUI表示を切り替える。
pub enum ConnCheckMsg {
    /// `/v1/models` 疎通成功、モデル一覧を取得できた。
    ModelsOk(Vec<String>),
    /// vision能力チェック完了（画像入力に反応した応答本文の先頭一部）。
    VisionOk(String),
    /// いずれかの段階で失敗（エラーメッセージ）。
    Failed(String),
}

fn http_client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("HTTPクライアント初期化失敗: {e}"))
}

fn fetch_models(client: &reqwest::blocking::Client, base_url: &str) -> Result<Vec<String>, String> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let resp = client.get(&url).send().map_err(|e| format!("接続失敗: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let parsed: ModelsListResp = resp.json().map_err(|e| format!("応答の解析に失敗: {e}"))?;
    Ok(parsed.data.into_iter().map(|m| m.id).collect())
}

/// `/v1/chat/completions`へ投げ、応答本文(content)を返す。vision能力チェック・実OCR
/// リクエスト・翻訳リクエストで共有する（contentパーツの組み立てだけ呼び出し側で変える）。
fn send_chat(
    client: &reqwest::blocking::Client,
    base_url: &str,
    model: &str,
    content: Vec<ChatContentPart>,
    max_tokens: Option<u32>,
) -> Result<String, String> {
    let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let req = ChatRequest {
        model,
        messages: vec![ChatMessage { role: "user", content }],
        max_tokens,
    };
    let resp = client.post(&url).json(&req).send().map_err(|e| format!("接続失敗: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let parsed: ChatResponse = resp.json().map_err(|e| format!("応答の解析に失敗: {e}"))?;
    let message = parsed.choices.into_iter().next().map(|c| c.message);
    let content = match message {
        Some(m) if !m.content.trim().is_empty() => m.content,
        // contentが空でも、reasoningフィールドに思考過程ごと出力が残っていることがある
        // （max_tokens到達等でcontentへコピーされる前に打ち切られたケースの救済）。
        Some(m) => m.reasoning.filter(|r| !r.trim().is_empty()).unwrap_or_default(),
        None => String::new(),
    };
    if content.trim().is_empty() {
        return Err("応答が空でした（入力形式に対応していない可能性、またはトークン上限到達）".to_string());
    }
    Ok(content)
}

fn text_and_image_content(prompt: &str, image_data_url: String) -> Vec<ChatContentPart<'_>> {
    vec![
        ChatContentPart::Text(ChatMessageContentText { kind: "text", text: prompt }),
        ChatContentPart::Image(ChatMessageContentImage { kind: "image_url", image_url: ImageUrl { url: image_data_url } }),
    ]
}

fn text_only_content(prompt: &str) -> Vec<ChatContentPart<'_>> {
    vec![ChatContentPart::Text(ChatMessageContentText { kind: "text", text: prompt })]
}

fn check_vision(client: &reqwest::blocking::Client, base_url: &str, model: &str) -> Result<String, String> {
    send_chat(
        client,
        base_url,
        model,
        text_and_image_content("この画像は何色？一言で答えて", format!("data:image/png;base64,{PROBE_IMAGE_PNG_BASE64}")),
        None,
    )
}

/// 接続チェックをバックグラウンドスレッドで実行し、進行に応じて `ConnCheckMsg` を送る。
/// 呼び出し側(egui)は毎フレーム `try_recv` してUIへ反映し、送信後は `ctx.request_repaint()` 済みなので
/// 結果到着時に再描画がかかる。
pub fn spawn_conn_check(ctx: egui::Context, base_url: String, model: String) -> mpsc::Receiver<ConnCheckMsg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let client = match http_client(Duration::from_secs(15)) {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(ConnCheckMsg::Failed(e));
                ctx.request_repaint();
                return;
            }
        };

        match fetch_models(&client, &base_url) {
            Ok(models) => {
                let _ = tx.send(ConnCheckMsg::ModelsOk(models));
                ctx.request_repaint();
            }
            Err(e) => {
                let _ = tx.send(ConnCheckMsg::Failed(format!("疎通チェック失敗: {e}")));
                ctx.request_repaint();
                return;
            }
        }

        if model.trim().is_empty() {
            // モデル未選択の場合はvisionチェックまでは行わず、疎通確認のみで終える。
            return;
        }

        match check_vision(&client, &base_url, &model) {
            Ok(content) => {
                let preview: String = content.chars().take(60).collect();
                let _ = tx.send(ConnCheckMsg::VisionOk(preview));
            }
            Err(e) => {
                let _ = tx.send(ConnCheckMsg::Failed(format!("vision確認失敗: {e}")));
            }
        }
        ctx.request_repaint();
    });
    rx
}

// ── OCRリクエスト(Phase 1) ──────────────────────────────────────────────

/// OCRリクエストのタイムアウト。ページ全体の解析は色確認より長くかかるため接続チェックより長め。
/// 実機検証で、モデルランナーがコールド状態(アイドルによるアンロード後の初回リクエスト等)だと
/// ロードだけで数十秒かかり、120秒では不足するケースを確認したため余裕を持たせている。
const OCR_REQUEST_TIMEOUT_SECS: u64 = 300;

/// OCRリクエストの応答トークン上限。reasoningモデルは画像解析の思考が長くなりがちで、
/// サーバ既定値のままだと最終回答(JSON配列)を出す前に打ち切られ空応答になったため、
/// 実機で空応答が再現した後に導入。
const OCR_MAX_TOKENS: u32 = 4096;

/// モデルへ渡すプロンプト。実機検証(qwen3.5:latest)で「説明文なし・コードフェンスなしの
/// JSON配列文字列」がそのまま`content`に返ることを確認済み。bboxは要求しない
/// （オーバーレイ表示は縦スクロールのテキスト一覧のみのため、座標情報は不要）。
///
/// 見開きは1枚の結合画像として渡さず、常に単独ページの画像を1枚ずつ渡す方式に変更した
/// （呼び出し側`viewer_host.rs`の`trigger_translate_ocr`参照）。結合画像1枚に対して
/// 「どこまでが左ページか」をモデルに自己申告させると境界判定が信頼できず、実機検証でも
/// 読み順の自問自答だけで思考トークンを使い切るケースを確認したため。ページの切り分け・
/// 「ページ数XX:」ラベル付けは常にアプリ側で行う。単独ページのコマ内読み順（右上開始、
/// 右→左・上→下）は綴じ方向に関係なく同じなので、page_modeの考慮は不要。
const OCR_PROMPT: &str = "この漫画ページ画像から、吹き出し内のテキストを自然な読み順（右上のコマから開始し、右→左・上→下の順、日本語漫画の一般的な順序）で抽出してください。出力は説明文なしで、各吹き出しのテキストを1要素とするJSON配列のみを返してください。例: [\"セリフ1\",\"セリフ2\"]";

/// OCR結果1ページぶん。
pub struct OcrPageResult {
    /// 読み順に並んだ吹き出しテキスト。
    pub lines: Vec<String>,
    /// true = JSON配列としてのパースに失敗し、応答本文を行分割しただけのフォールバック。
    pub raw_fallback: bool,
}

pub enum OcrMsg {
    Result(OcrPageResult),
    Failed(String),
}

/// モデル応答本文から文字列配列を取り出す。まずJSON配列としてパースを試み、失敗したら
/// 応答中の最初の`[`〜最後の`]`を抜き出して再試行（コードフェンス等の前後余分な文字に対処）、
/// それでも失敗したら非空行への単純分割にフォールバックする。戻り値の2番目はフォールバックか否か。
/// OCR・翻訳の両リクエストで応答形式(説明文なしJSON配列)を共通にしているため共有する。
fn parse_string_array_or_lines(content: &str) -> (Vec<String>, bool) {
    let trimmed = content.trim();
    if let Ok(lines) = serde_json::from_str::<Vec<String>>(trimmed) {
        return (lines, false);
    }
    if let (Some(start), Some(end)) = (trimmed.find('['), trimmed.rfind(']')) {
        if start < end {
            if let Ok(lines) = serde_json::from_str::<Vec<String>>(&trimmed[start..=end]) {
                return (lines, false);
            }
        }
    }
    let lines: Vec<String> = trimmed
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    (lines, true)
}

fn parse_ocr_content(content: &str) -> OcrPageResult {
    let (lines, raw_fallback) = parse_string_array_or_lines(content);
    OcrPageResult { lines, raw_fallback }
}

/// `image::DynamicImage` をPNGへエンコードしてBase64文字列にする（data URL用）。
fn encode_image_png_base64(image: &image::DynamicImage) -> Result<String, String> {
    let mut buf = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| format!("画像エンコード失敗: {e}"))?;
    Ok(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &buf))
}

/// 1ページぶんのOCRリクエストをバックグラウンドスレッドで実行する。常に単独ページの
/// 画像1枚を渡す（見開きの2ページ分割・逐次実行は呼び出し側`viewer_host.rs`が行う）。
pub fn spawn_ocr_request(ctx: egui::Context, base_url: String, model: String, image: image::DynamicImage) -> mpsc::Receiver<OcrMsg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<OcrPageResult, String> {
            let data_url_body = encode_image_png_base64(&image)?;
            let client = http_client(Duration::from_secs(OCR_REQUEST_TIMEOUT_SECS))?;
            let content = send_chat(&client, &base_url, &model, text_and_image_content(OCR_PROMPT, format!("data:image/png;base64,{data_url_body}")), Some(OCR_MAX_TOKENS))?;
            Ok(parse_ocr_content(&content))
        })();
        match result {
            Ok(page) => { let _ = tx.send(OcrMsg::Result(page)); }
            Err(e) => { let _ = tx.send(OcrMsg::Failed(e)); }
        }
        ctx.request_repaint();
    });
    rx
}

// ── 翻訳リクエスト(Phase 6-B) ────────────────────────────────────────────

/// 翻訳リクエストのタイムアウト。画像を含まないテキスト専用リクエストなのでOCRより短めでよい。
const TRANSLATE_REQUEST_TIMEOUT_SECS: u64 = 120;
/// 翻訳リクエストの応答トークン上限。OCRと同じくreasoningモデルの保険として大きめにしておく。
const TRANSLATE_MAX_TOKENS: u32 = 4096;

fn translate_lang_prompt_name(lang: TranslateLang) -> &'static str {
    match lang {
        TranslateLang::Japanese => "Japanese (日本語)",
        TranslateLang::ChineseSimplified => "Simplified Chinese (简体中文)",
        TranslateLang::ChineseTraditional => "Traditional Chinese (繁體中文)",
        TranslateLang::English => "English",
        TranslateLang::Korean => "Korean (한국어)",
    }
}

/// 原文言語・翻訳先言語ごとに変わる部分だけプロンプトへ差し込む。原文はOCR結果の行配列を
/// そのままJSON化して埋め込み、モデルには「同じ要素数・同じ順序で翻訳したJSON配列のみ」を
/// 求める（OCRの読み順維持と同じ考え方で、モデルに構造判断をさせない）。
fn build_translate_prompt(lines: &[String], source: TranslateLang, target: TranslateLang) -> String {
    let source_lang_name = translate_lang_prompt_name(source);
    let target_lang_name = translate_lang_prompt_name(target);
    let source_json = serde_json::to_string(lines).unwrap_or_default();
    format!(
        "以下は{source_lang_name}の漫画の吹き出しテキストをJSON配列にしたものです。各要素を{target_lang_name}へ翻訳してください。\
出力は説明文なしで、原文と同じ要素数・同じ順序のJSON配列のみを返してください。\
原文: {source_json}"
    )
}

/// 翻訳結果1ページぶん。
pub struct TranslatePageResult {
    /// OCR原文と同じ順序に対応した翻訳済みテキスト一覧。
    pub lines: Vec<String>,
    /// true = JSON配列としてのパースに失敗し、応答本文を行分割しただけのフォールバック。
    pub raw_fallback: bool,
}

pub enum TranslateMsg {
    Result(TranslatePageResult),
    Failed(String),
}

fn parse_translate_content(content: &str) -> TranslatePageResult {
    let (lines, raw_fallback) = parse_string_array_or_lines(content);
    TranslatePageResult { lines, raw_fallback }
}

/// 1ページぶんの翻訳リクエストをバックグラウンドスレッドで実行する。画像は送らず、
/// OCR原文の行配列だけをテキストとして渡す（OCRと翻訳は完全に独立した処理単位）。
pub fn spawn_translate_request(ctx: egui::Context, base_url: String, model: String, lines: Vec<String>, source: TranslateLang, target: TranslateLang) -> mpsc::Receiver<TranslateMsg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<TranslatePageResult, String> {
            let client = http_client(Duration::from_secs(TRANSLATE_REQUEST_TIMEOUT_SECS))?;
            let prompt = build_translate_prompt(&lines, source, target);
            let content = send_chat(&client, &base_url, &model, text_only_content(&prompt), Some(TRANSLATE_MAX_TOKENS))?;
            Ok(parse_translate_content(&content))
        })();
        match result {
            Ok(page) => { let _ = tx.send(TranslateMsg::Result(page)); }
            Err(e) => { let _ = tx.send(TranslateMsg::Failed(e)); }
        }
        ctx.request_repaint();
    });
    rx
}

// ── 原文言語判定(Phase 6) ────────────────────────────────────────────────
// OCR自体は言語を判定せず読み取るだけ（読み順プロンプトが言語非依存のため）。
// 「言語判定」ボタンが押されたときだけ、OCR原文を翻訳モデルへ渡して言語コードを
// 1つ返させる。自動実行はしない（ユーザーが明示的に押した時のみ）。

fn build_lang_detect_prompt(lines: &[String]) -> String {
    let source_json = serde_json::to_string(lines).unwrap_or_default();
    format!(
        "以下は漫画の吹き出しから抽出したテキストをJSON配列にしたものです。このテキスト全体が\
何語で書かれているか判定してください。出力は説明文なしで、次のいずれか1つの言語コードのみを\
返してください: ja, zh-Hans, zh-Hant, en, ko\
テキスト: {source_json}"
    )
}

fn parse_lang_detect_content(content: &str) -> Option<TranslateLang> {
    let trimmed = content.trim();
    if let Some(lang) = translate_lang_from_code(trimmed) {
        return Some(lang);
    }
    // 説明文が混ざった応答へのフォールバック。zh-Hans/zh-Hantを先に見る
    // （"en"等の短いコードが長いコードの一部に誤マッチしないようにするため）。
    for code in ["zh-Hans", "zh-Hant", "ja", "en", "ko"] {
        if trimmed.contains(code) {
            return translate_lang_from_code(code);
        }
    }
    None
}

pub enum LangDetectMsg {
    Result(TranslateLang),
    Failed(String),
}

/// OCR原文の言語判定リクエストをバックグラウンドスレッドで実行する。
/// 翻訳リクエストと同じモデル(translation_model)・エンドポイントを使う。
pub fn spawn_lang_detect_request(ctx: egui::Context, base_url: String, model: String, lines: Vec<String>) -> mpsc::Receiver<LangDetectMsg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<TranslateLang, String> {
            let client = http_client(Duration::from_secs(TRANSLATE_REQUEST_TIMEOUT_SECS))?;
            let prompt = build_lang_detect_prompt(&lines);
            let content = send_chat(&client, &base_url, &model, text_only_content(&prompt), Some(TRANSLATE_MAX_TOKENS))?;
            parse_lang_detect_content(&content).ok_or_else(|| "言語判定結果を解釈できませんでした".to_string())
        })();
        match result {
            Ok(lang) => { let _ = tx.send(LangDetectMsg::Result(lang)); }
            Err(e) => { let _ = tx.send(LangDetectMsg::Failed(e)); }
        }
        ctx.request_repaint();
    });
    rx
}

// ── OCRテキストの永続化(Phase 3) ──────────────────────────────────────────
// redbへのBLOB登録ではなく、フォルダのキャッシュディレクトリ(サムネDBの脇)に
// アーカイブ名のサブフォルダを掘り、ページごとに素のtxtとして書き出す方式にした。
// 理由: ユーザーがOSのファイラーで直接txtを開いて手動でノイズ取り（OCR誤読の訂正・
// 整形）できるようにするため。この編集後のtxtを「翻訳の原本」として扱いたいという
// 要望があり、redb格納だと編集導線・エクスポート導線をこちらで別途作り込む必要が
// あったが、txt直置きならOSのファイラー・エディタがそのまま導線になる。

/// アーカイブ1本ぶんのOCR txtを置くフォルダ（サムネ等のcache.redbと同じ
/// キャッシュディレクトリ配下）。まだ存在しなくてもパスだけ返す（作成しない）。
pub fn ocr_text_dir(neko_dir: &Path, archive_filename: &str) -> PathBuf {
    neko_dir.join("ocr_text").join(archive_filename)
}

fn ocr_text_path(neko_dir: &Path, archive_filename: &str, original_index: usize) -> PathBuf {
    ocr_text_dir(neko_dir, archive_filename).join(format!("{original_index:04}.txt"))
}

/// OCR結果をページ単位のtxtとして保存する（既存があれば上書き）。
/// 上書きなので、手動編集済みのtxtに対して再実行すると編集内容は失われる
/// （「原本を作り直す」操作として扱う）。
pub fn save_ocr_text(neko_dir: &Path, archive_filename: &str, original_index: usize, lines: &[String]) -> std::io::Result<()> {
    let path = ocr_text_path(neko_dir, archive_filename, original_index);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, lines.join("\n"))
}

/// 保存済みのOCR txtを読み込む（ユーザーが手編集した内容もそのまま反映される）。
/// 存在しなければ None（＝未実行扱い）。
pub fn load_ocr_text(neko_dir: &Path, archive_filename: &str, original_index: usize) -> Option<Vec<String>> {
    let path = ocr_text_path(neko_dir, archive_filename, original_index);
    let content = std::fs::read_to_string(path).ok()?;
    Some(content.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
}

/// アーカイブ内に1P分でもOCR txtが残っているか（OCR/翻訳子ウィンドウの自動オープン判定用）。
pub fn has_any_ocr_text(neko_dir: &Path, archive_filename: &str) -> bool {
    let dir = ocr_text_dir(neko_dir, archive_filename);
    let Ok(entries) = std::fs::read_dir(&dir) else { return false };
    entries.filter_map(|e| e.ok()).any(|e| e.path().extension().is_some_and(|ext| ext == "txt"))
}

// ── 翻訳結果テキストの永続化(Phase 6-C) ────────────────────────────────────
// OCR txtと同じ命名規則(archive_filename/{index:04}.txt)を、"translated_text"という
// 別の固定フォルダ名の下にそのまま踏襲する。言語別には分けず、直近に翻訳した内容で
// 上書きする（OCR原本と同じ「最新版だけ保持」方式）。

pub fn translated_text_dir(neko_dir: &Path, archive_filename: &str) -> PathBuf {
    neko_dir.join("translated_text").join(archive_filename)
}

fn translated_text_path(neko_dir: &Path, archive_filename: &str, original_index: usize) -> PathBuf {
    translated_text_dir(neko_dir, archive_filename).join(format!("{original_index:04}.txt"))
}

pub fn save_translated_text(neko_dir: &Path, archive_filename: &str, original_index: usize, lines: &[String]) -> std::io::Result<()> {
    let path = translated_text_path(neko_dir, archive_filename, original_index);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // 0000.txtの1行目はアーカイブ全体の言語ペアを記録するメタ行として使う場所なので、
    // 既にあれば保持したまま本文だけ上書きする（save_translate_lang_meta参照）。
    let existing_meta_line = if original_index == 0 {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| content.lines().next().map(str::to_string))
            .filter(|line| line.starts_with(TRANSLATE_LANG_META_PREFIX))
    } else {
        None
    };
    let mut out: Vec<String> = existing_meta_line.into_iter().collect();
    out.extend(lines.iter().cloned());
    std::fs::write(path, out.join("\n"))
}

pub fn load_translated_text(neko_dir: &Path, archive_filename: &str, original_index: usize) -> Option<Vec<String>> {
    let path = translated_text_path(neko_dir, archive_filename, original_index);
    let content = std::fs::read_to_string(path).ok()?;
    Some(
        content
            .lines()
            .filter(|line| !line.starts_with(TRANSLATE_LANG_META_PREFIX))
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// 翻訳言語ペアのメタ行に付ける目印。手動編集者が見ても「システムが書いた行」と
/// 分かるよう先頭にコメント記号+アプリ名を置く。
const TRANSLATE_LANG_META_PREFIX: &str = "# [Nekoviewer] ";

fn translate_lang_code(lang: TranslateLang) -> &'static str {
    match lang {
        TranslateLang::Japanese => "ja",
        TranslateLang::ChineseSimplified => "zh-Hans",
        TranslateLang::ChineseTraditional => "zh-Hant",
        TranslateLang::English => "en",
        TranslateLang::Korean => "ko",
    }
}

fn translate_lang_from_code(code: &str) -> Option<TranslateLang> {
    match code {
        "ja" => Some(TranslateLang::Japanese),
        "zh-Hans" => Some(TranslateLang::ChineseSimplified),
        "zh-Hant" => Some(TranslateLang::ChineseTraditional),
        "en" => Some(TranslateLang::English),
        "ko" => Some(TranslateLang::Korean),
        _ => None,
    }
}

fn format_translate_lang_meta_line(source: TranslateLang, target: TranslateLang) -> String {
    format!(
        "{TRANSLATE_LANG_META_PREFIX}このアーカイブを翻訳した際の言語設定を自動記録しています(lang={}:{})。削除しても翻訳結果自体には影響しません。",
        translate_lang_code(source),
        translate_lang_code(target)
    )
}

fn parse_translate_lang_meta_line(line: &str) -> Option<(TranslateLang, TranslateLang)> {
    let after = line.split_once("lang=")?.1;
    let pair = after.split(')').next()?;
    let (src, dst) = pair.split_once(':')?;
    Some((translate_lang_from_code(src)?, translate_lang_from_code(dst)?))
}

/// アーカイブ単位の翻訳言語ペアを記録する。0000.txtの1行目をメタ行として
/// 追加/更新する（本文（翻訳結果）が既にあれば保持する）。
pub fn save_translate_lang_meta(neko_dir: &Path, archive_filename: &str, source: TranslateLang, target: TranslateLang) -> std::io::Result<()> {
    let path = translated_text_path(neko_dir, archive_filename, 0);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let existing = std::fs::read_to_string(&path).ok();
    let body_lines: Vec<&str> = existing
        .as_deref()
        .map(|content| {
            let mut lines: Vec<&str> = content.lines().collect();
            if lines.first().is_some_and(|l| l.starts_with(TRANSLATE_LANG_META_PREFIX)) {
                lines.remove(0);
            }
            lines
        })
        .unwrap_or_default();
    let meta_line = format_translate_lang_meta_line(source, target);
    let mut out = vec![meta_line];
    out.extend(body_lines.into_iter().map(str::to_string));
    std::fs::write(path, out.join("\n"))
}

/// 保存済みの翻訳言語ペアを読み込む。0000.txtが無い、またはメタ行が無ければNone
/// （＝未翻訳、またはPhase4以前に保存されたデータ）。
pub fn load_translate_lang_meta(neko_dir: &Path, archive_filename: &str) -> Option<(TranslateLang, TranslateLang)> {
    let path = translated_text_path(neko_dir, archive_filename, 0);
    let content = std::fs::read_to_string(path).ok()?;
    let first_line = content.lines().next()?;
    parse_translate_lang_meta_line(first_line)
}

/// OS標準のファイラーでフォルダを開く（ベストエフォート、失敗しても無視する）。
pub fn open_in_file_manager(path: &Path) {
    let _ = std::fs::create_dir_all(path);
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

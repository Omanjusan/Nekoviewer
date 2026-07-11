//! 翻訳機能(実験的)。現段階ではOCR抽出のみ。ローカルAI(Ollama/OpenWebUI等の
//! OpenAI互換API)へHTTPで接続し、疎通確認・vision能力確認・OCRリクエストを行う。
//! クラウドAPIは対象外（APIキー管理は今回のスコープ外）。

use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 半透明オーバーレイウィンドウの配置（ビューアー画面を軸とした四隅）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OverlayCorner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

pub fn overlay_corner_to_str(c: OverlayCorner) -> &'static str {
    match c {
        OverlayCorner::TopLeft => "top_left",
        OverlayCorner::TopRight => "top_right",
        OverlayCorner::BottomLeft => "bottom_left",
        OverlayCorner::BottomRight => "bottom_right",
    }
}

pub fn parse_overlay_corner(s: &str) -> OverlayCorner {
    match s {
        "top_left" => OverlayCorner::TopLeft,
        "bottom_left" => OverlayCorner::BottomLeft,
        "bottom_right" => OverlayCorner::BottomRight,
        _ => OverlayCorner::TopRight,
    }
}

/// 「翻訳機能」設定タブで編集する永続設定。
#[derive(Clone)]
pub struct TranslateConfig {
    /// OpenAI互換APIのベースURL（例: http://172.17.0.1:11434）。空文字 = 未設定。
    pub base_url: String,
    /// 使用するモデル名（例: qwen3.5:latest）。
    pub model: String,
    /// オーバーレイウィンドウの横幅(px)。
    pub overlay_width: u32,
    /// オーバーレイウィンドウの配置(四隅)。
    pub overlay_corner: OverlayCorner,
}

impl Default for TranslateConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: String::new(),
            overlay_width: 360,
            overlay_corner: OverlayCorner::TopRight,
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

/// 画像1枚 + プロンプトを`/v1/chat/completions`へ投げ、応答本文(content)を返す。
/// vision能力チェック・実OCRリクエストの両方から共有する。
fn send_chat_with_image(
    client: &reqwest::blocking::Client,
    base_url: &str,
    model: &str,
    prompt: &str,
    image_data_url: String,
) -> Result<String, String> {
    let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let req = ChatRequest {
        model,
        messages: vec![ChatMessage {
            role: "user",
            content: vec![
                ChatContentPart::Text(ChatMessageContentText { kind: "text", text: prompt }),
                ChatContentPart::Image(ChatMessageContentImage {
                    kind: "image_url",
                    image_url: ImageUrl { url: image_data_url },
                }),
            ],
        }],
    };
    let resp = client.post(&url).json(&req).send().map_err(|e| format!("接続失敗: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let parsed: ChatResponse = resp.json().map_err(|e| format!("応答の解析に失敗: {e}"))?;
    let content = parsed
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .unwrap_or_default();
    if content.trim().is_empty() {
        return Err("応答が空でした（画像入力に対応していない可能性）".to_string());
    }
    Ok(content)
}

fn check_vision(client: &reqwest::blocking::Client, base_url: &str, model: &str) -> Result<String, String> {
    send_chat_with_image(
        client,
        base_url,
        model,
        "この画像は何色？一言で答えて",
        format!("data:image/png;base64,{PROBE_IMAGE_PNG_BASE64}"),
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
const OCR_REQUEST_TIMEOUT_SECS: u64 = 120;

/// モデルへ渡すプロンプト。実機検証(qwen3.5:latest)で「説明文なし・コードフェンスなしの
/// JSON配列文字列」がそのまま`content`に返ることを確認済み。bboxは要求しない
/// （オーバーレイ表示は縦スクロールのテキスト一覧のみのため、座標情報は不要）。
const OCR_PROMPT: &str = "この漫画ページ画像から、吹き出し内のテキストを自然な読み順（右上から左下、日本語漫画の一般的な順序）で抽出してください。出力は説明文なしで、各吹き出しのテキストを1要素とするJSON配列のみを返してください。例: [\"セリフ1\",\"セリフ2\"]";

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

/// モデル応答本文からOCR結果を取り出す。まずJSON配列としてパースを試み、失敗したら
/// 応答中の最初の`[`〜最後の`]`を抜き出して再試行（コードフェンス等の前後余分な文字に対処）、
/// それでも失敗したら非空行への単純分割にフォールバックする。
fn parse_ocr_content(content: &str) -> OcrPageResult {
    let trimmed = content.trim();
    if let Ok(lines) = serde_json::from_str::<Vec<String>>(trimmed) {
        return OcrPageResult { lines, raw_fallback: false };
    }
    if let (Some(start), Some(end)) = (trimmed.find('['), trimmed.rfind(']')) {
        if start < end {
            if let Ok(lines) = serde_json::from_str::<Vec<String>>(&trimmed[start..=end]) {
                return OcrPageResult { lines, raw_fallback: false };
            }
        }
    }
    let lines: Vec<String> = trimmed
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    OcrPageResult { lines, raw_fallback: true }
}

/// `image::DynamicImage` をPNGへエンコードしてBase64文字列にする（data URL用）。
fn encode_image_png_base64(image: &image::DynamicImage) -> Result<String, String> {
    let mut buf = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| format!("画像エンコード失敗: {e}"))?;
    Ok(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &buf))
}

/// 1ページぶんのOCRリクエストをバックグラウンドスレッドで実行する。
pub fn spawn_ocr_request(ctx: egui::Context, base_url: String, model: String, image: image::DynamicImage) -> mpsc::Receiver<OcrMsg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<OcrPageResult, String> {
            let data_url_body = encode_image_png_base64(&image)?;
            let client = http_client(Duration::from_secs(OCR_REQUEST_TIMEOUT_SECS))?;
            let content = send_chat_with_image(&client, &base_url, &model, OCR_PROMPT, format!("data:image/png;base64,{data_url_body}"))?;
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

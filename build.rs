// Windows向けビルド時にexeへアイコンリソースを埋め込む。
// 非Windowsターゲットではembed_resource::compileが自動的に何もしない。
fn main() {
    embed_resource::compile("packaging/windows/nekoviewer.rc", embed_resource::NONE);
}

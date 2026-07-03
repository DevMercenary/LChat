// Встраиваем иконку в исполняемый файл на Windows (значок в панели задач и в проводнике).
// На других ОС build-скрипт ничего не делает.
fn main() {
    #[cfg(windows)]
    {
        let ico = "packaging/io.github.DevMercenary.LChat.ico";
        println!("cargo:rerun-if-changed={ico}");
        let mut res = winresource::WindowsResource::new();
        res.set_icon(ico);
        // Не валим сборку, если встроить ресурс не удалось — exe всё равно соберётся.
        if let Err(e) = res.compile() {
            println!("cargo:warning=не удалось встроить иконку: {e}");
        }
    }
}

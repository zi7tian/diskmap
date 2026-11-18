//! diskmap-cli 可执行文件。逻辑都在库里（src/lib.rs），桌面版也会调用。

fn main() {
    std::process::exit(diskmap_cli::run(std::env::args_os().collect()));
}

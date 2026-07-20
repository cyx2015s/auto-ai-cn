use indicatif::MultiProgress;
use std::sync::LazyLock;

/// 全局进度条管理器，任何模块都可直接使用
pub static MULTI: LazyLock<MultiProgress> = LazyLock::new(MultiProgress::new);

/// 创建 spinner 进度条并注册到全局 MultiProgress
pub fn new_spinner() -> indicatif::ProgressBar {
    let pb = MULTI.add(indicatif::ProgressBar::new_spinner());
    pb.set_style(
        indicatif::ProgressStyle::with_template("{spinner} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    pb
}

/// 创建条形进度条并注册到全局 MultiProgress
pub fn new_bar() -> indicatif::ProgressBar {
    let pb = MULTI.add(indicatif::ProgressBar::new(0));
    pb.set_style(
        indicatif::ProgressStyle::with_template("{bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("##-"),
    );
    pb
}
